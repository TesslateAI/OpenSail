/*
 * voie-activation-launch: control-side exec shim for the activation child.
 *
 * The voie-cloud service execs this program (via VOIE_ACTIVATION_ENTRY /
 * VOIE_NODE wiring in nix/modules/control.nix) instead of Node.js directly.
 * It hands the single inherited bridge socket (fd 3, the parent<->child
 * protocol endpoint) to the voie-activation broker over a restricted UNIX
 * socket, then blocks until the broker reports the real child exit status,
 * so the parent's wait() semantics and timeout kill behavior are preserved:
 *
 *   - parent kill of this process closes the broker connection, and the
 *     broker responds by killing the activation child (supervision chain);
 *   - broker absence or any protocol fault exits nonzero immediately,
 *     which the parent surfaces as an activation failure (fail closed).
 *
 * This process never touches credentials: it forwards one descriptor and
 * one path, and reads back one status byte.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/un.h>
#include <unistd.h>

/* Must match activation-broker.c and the socket unit in control.nix. */
#define SPAWN_SOCK_PATH "/run/voie/activation/spawn.sock"
#define BRIDGE_FD 3
#define ENTRY_MAX 4064
/* "VOIACT1\0" — versioned handshake so a mismatched broker refuses us. */
static const unsigned char REQUEST_MAGIC[8] = {
  0x56, 0x4F, 0x49, 0x41, 0x43, 0x54, 0x31, 0x00
};

static int
fail (const char *what)
{
  fprintf (stderr, "voie-activation-launch: %s: %s\n", what,
           strerror (errno));
  return 98;
}

static int
bad (const char *what)
{
  fprintf (stderr, "voie-activation-launch: %s\n", what);
  return 98;
}

int
main (int argc, char **argv)
{
  if (argc != 2)
    return bad ("usage: voie-activation-launch ENTRY");
  const char *entry = argv[1];
  if (entry[0] != '/')
    return bad ("entry path must be absolute");
  size_t entry_len = strnlen (entry, ENTRY_MAX + 1);
  if (entry_len == 0 || entry_len > ENTRY_MAX)
    return bad ("entry path length out of bounds");

  /* The parent hands us exactly one thing of value: the bridge socket on
   * fd 3. Anything else is a boundary violation before we even dial. */
  struct stat st;
  if (fstat (BRIDGE_FD, &st) < 0)
    return fail ("fstat bridge fd");
  if (!S_ISSOCK (st.st_mode))
    return bad ("bridge fd 3 is not a socket");
  if (stat (entry, &st) < 0)
    return fail ("stat entry");
  if (!S_ISREG (st.st_mode))
    return bad ("entry is not a regular file");

  signal (SIGPIPE, SIG_IGN);

  int s = socket (AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
  if (s < 0)
    return fail ("socket");
  struct sockaddr_un addr;
  memset (&addr, 0, sizeof (addr));
  addr.sun_family = AF_UNIX;
  if (sizeof (SPAWN_SOCK_PATH) > sizeof (addr.sun_path))
    return bad ("spawn socket path too long");
  memcpy (addr.sun_path, SPAWN_SOCK_PATH, sizeof (SPAWN_SOCK_PATH));
  if (connect (s, (struct sockaddr *) &addr, sizeof (addr)) < 0)
    return fail ("connect " SPAWN_SOCK_PATH);

  /* One request: magic + NUL-terminated entry path, plus one SCM_RIGHTS
   * descriptor (the bridge). Nothing else crosses this boundary. */
  unsigned char buf[8 + ENTRY_MAX + 1];
  memcpy (buf, REQUEST_MAGIC, sizeof (REQUEST_MAGIC));
  memcpy (buf + 8, entry, entry_len + 1);
  size_t total = 8 + entry_len + 1;

  union
  {
    char buf[CMSG_SPACE (sizeof (int))];
    struct cmsghdr align;
  } u;
  memset (&u, 0, sizeof (u));

  struct iovec iov = {.iov_base = buf,.iov_len = total };
  struct msghdr mh;
  memset (&mh, 0, sizeof (mh));
  mh.msg_iov = &iov;
  mh.msg_iovlen = 1;
  mh.msg_control = u.buf;
  mh.msg_controllen = sizeof (u.buf);
  struct cmsghdr *cm = CMSG_FIRSTHDR (&mh);
  cm->cmsg_level = SOL_SOCKET;
  cm->cmsg_type = SCM_RIGHTS;
  cm->cmsg_len = CMSG_LEN (sizeof (int));
  int send_fd = BRIDGE_FD;
  memcpy (CMSG_DATA (cm), &send_fd, sizeof (int));

  size_t sent = 0;
  while (sent < total)
    {
      ssize_t n = sendmsg (s, &mh, MSG_NOSIGNAL);
      if (n <= 0)
        {
          if (n < 0 && errno == EINTR)
            continue;
          return fail ("sendmsg to activation broker");
        }
      /* The rights fragment accompanies the first byte only; afterwards
       * fall back to a plain write for any remainder. */
      iov.iov_base = (char *) iov.iov_base + n;
      iov.iov_len -= n;
      sent += (size_t) n;
      mh.msg_control = NULL;
      mh.msg_controllen = 0;
    }

  /* Block until the broker reports the child exit status. If the parent
   * kills us first (activation timeout), the socket closes and the broker
   * kills the child; we never swallow that into a fake success. */
  unsigned char status = 98;
  ssize_t n;
  while ((n = read (s, &status, 1)) < 0)
    {
      if (errno != EINTR)
        return fail ("read status from activation broker");
    }
  close (s);
  if (n == 0)
    return bad ("broker closed without a child status");
  return (int) status;
}
