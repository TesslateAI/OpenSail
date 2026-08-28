/*
 * voie-activation-broker: privileged-handoff executor for activation
 * children, running as the dedicated voie-activation account.
 *
 * systemd socket-activation hands us one accepted connection on fd 0 (the
 * listener is mode 0660 root:voie-cloud, so only the control service can
 * dial it). The protocol is exactly:
 *
 *   request : 8-byte magic "VOIACT1\0" + NUL-terminated entry path,
 *             plus one SCM_RIGHTS descriptor (the parent<->child bridge)
 *   response: 1 byte = child exit status
 *
 * We exec the pinned Node.js binary with unit-supplied Node flags followed
 * by the received entry argument, an environment of exactly
 * HOME/LANG/PATH/TMPDIR (matching the
 * parent-side attestation contract), stdio 0/1/2 on /dev/null, and the
 * bridge at fd 3. The entry must live under /nix/store/. While the child
 * runs we watch the connection: if the launcher disappears (parent timeout
 * kill), the child is killed.
 *
 * No privilege transition happens anywhere: this unit is already the
 * target identity; it merely receives a descriptor. It cannot read
 * /etc/voie/secrets or /etc/voie/control.env (root:voie-cloud 0750/0640),
 * so activation children never see control credentials.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define REQUEST_MAGIC_LEN 8
#define ENTRY_MAX 4064
#define BRIDGE_FD 3
#define HOME_DIR "/var/lib/voie-activation/home"
#define HOME_TPL HOME_DIR "/voie-act-XXXXXX"
/* Hard ceiling even without a launcher disconnect: well above the
 * parent-side CHILD_TIMEOUT so the parent stays the primary supervisor. */
#define CHILD_HARD_CAP_SECS 3600

static const unsigned char REQUEST_MAGIC[REQUEST_MAGIC_LEN] = {
  0x56, 0x4F, 0x49, 0x41, 0x43, 0x54, 0x31, 0x00
};

static int
bad (const char *what)
{
  fprintf (stderr, "voie-activation-broker: %s\n", what);
  return 98;
}

static int
fail (const char *what)
{
  fprintf (stderr, "voie-activation-broker: %s: %s\n", what,
           strerror (errno));
  return 98;
}

int
main (int argc, char **argv)
{
  if (argc < 3)
    return bad ("usage: voie-activation-broker NODE_BIN [NODE_ARG...] CHILD_PATH");
  const char *node_bin = argv[1];
  const char *child_path = argv[argc - 1];

  signal (SIGPIPE, SIG_IGN);

  /* Connection socket arrives on fd 0 (systemd Accept=yes). */
  struct stat st;
  if (fstat (0, &st) < 0 || !S_ISSOCK (st.st_mode))
    return bad ("fd 0 is not an accepted connection");

  /* Receive magic + entry path + exactly one SCM_RIGHTS descriptor. */
  unsigned char buf[REQUEST_MAGIC_LEN + ENTRY_MAX + 1];
  int bridge_fd = -1;
  size_t got = 0;
  while (got < sizeof (buf))
    {
      union
      {
        char buf[CMSG_SPACE (sizeof (int))];
        struct cmsghdr align;
      } u;
      memset (&u, 0, sizeof (u));
      struct iovec iov = {.iov_base = buf + got,.iov_len = sizeof (buf) - got };
      struct msghdr mh;
      memset (&mh, 0, sizeof (mh));
      mh.msg_iov = &iov;
      mh.msg_iovlen = 1;
      mh.msg_control = u.buf;
      mh.msg_controllen = sizeof (u.buf);
      ssize_t n = recvmsg (0, &mh, MSG_CMSG_CLOEXEC);
      if (n <= 0)
        {
          if (n < 0 && errno == EINTR)
            continue;
          return fail ("recvmsg request");
        }
      for (struct cmsghdr * cm = CMSG_FIRSTHDR (&mh); cm;
           cm = CMSG_NXTHDR (&mh, cm))
        {
          if (cm->cmsg_level == SOL_SOCKET && cm->cmsg_type == SCM_RIGHTS
              && cm->cmsg_len == CMSG_LEN (sizeof (int)))
            {
              if (bridge_fd >= 0)
                return bad ("more than one descriptor received");
              memcpy (&bridge_fd, CMSG_DATA (cm), sizeof (int));
            }
        }
      got += (size_t) n;
      /* One sendmsg carries everything; stop after the first record that
       * completes the magic + path framing. */
      if (got >= REQUEST_MAGIC_LEN &&
          memchr (buf + REQUEST_MAGIC_LEN, '\0', got - REQUEST_MAGIC_LEN))
        break;
    }
  if (bridge_fd < 0)
    return bad ("no bridge descriptor received");
  if (memcmp (buf, REQUEST_MAGIC, REQUEST_MAGIC_LEN) != 0)
    return bad ("request magic mismatch");
  const char *entry = (const char *) (buf + REQUEST_MAGIC_LEN);
  size_t entry_len = strnlen (entry, ENTRY_MAX + 1);
  if (entry_len == 0 || entry_len > ENTRY_MAX)
    return bad ("entry path length out of bounds");
  if (strncmp (entry, "/nix/store/", strlen ("/nix/store/")) != 0)
    return bad ("entry is not a Nix store path");
  if (fstat (bridge_fd, &st) < 0 || !S_ISSOCK (st.st_mode))
    return bad ("received bridge fd is not a socket");
  if (stat (node_bin, &st) < 0 || !S_ISREG (st.st_mode) ||
      !(st.st_mode & S_IXUSR))
    return bad ("pinned node binary missing");

  /* Per-child scratch home owned by this identity; the parent-supplied
   * HOME/TMPDIR point into voie-cloud space and are never honored. */
  char home_tpl[] = HOME_TPL;
  char *home = mkdtemp (home_tpl);
  if (home == NULL)
    return fail ("mkdtemp activation home");

  pid_t pid = fork ();
  if (pid < 0)
    return fail ("fork");

  /* Final child argv: NODE_BIN <NODE_ARG…> ENTRY. Unit-supplied Node
   * flags sit between the binary and CHILD_PATH; extra Node flags are
   * supplied by the unit file so the hardened mode stays visible in the
   * broker argv (--jitless keeps Node off executable memory, which
   * MemoryDenyWriteExecute requires). */
  char **nargv = calloc ((size_t) argc, sizeof *nargv);
  if (nargv == NULL)
    return fail ("calloc node argv");
  nargv[0] = (char *) node_bin;
  for (int i = 2; i < argc - 1; i++)
    nargv[i - 1] = argv[i];
  nargv[argc - 2] = (char *) entry;

  if (pid == 0)
    {
      /* Child: exact boundary the parent attests against. */
      int nul = open ("/dev/null", O_RDWR);
      if (nul < 0)
        _exit (127);
      /* Stdio must not be sockets: systemd StandardError=journal makes
       * fd 2 a journal AF_UNIX socket, which the child attests against
       * and refuses as an unexpected inherited descriptor. */
      if (dup2 (nul, 0) < 0 || dup2 (nul, 1) < 0 || dup2 (nul, 2) < 0)
        _exit (127);
      close (nul);
      if (bridge_fd != BRIDGE_FD)
        {
          if (dup2 (bridge_fd, BRIDGE_FD) < 0)
            _exit (127);
          close (bridge_fd);
        }
      else
        {
          int flags = fcntl (BRIDGE_FD, F_GETFD);
          if (flags < 0 || fcntl (BRIDGE_FD, F_SETFD, flags & ~FD_CLOEXEC) < 0)
            _exit (127);
        }
      for (int fd = BRIDGE_FD + 1; fd < 4096; fd++)
        close (fd);
      if (chdir (home) < 0)
        _exit (127);
      extern char **environ;
      *environ = NULL;
      setenv ("HOME", home, 1);
      setenv ("TMPDIR", home, 1);
      setenv ("LANG", "C", 1);
      setenv ("PATH", child_path, 1);
      execv (node_bin, nargv);
      _exit (127);
    }

  close (bridge_fd);

  /* Supervise: report the real exit status through the launcher; if the
   * launcher vanishes or misbehaves, the child dies with it. */
  int code = 98;
  time_t deadline = time (NULL) + CHILD_HARD_CAP_SECS;
  for (;;)
    {
      int wst = 0;
      pid_t r = waitpid (pid, &wst, WNOHANG);
      if (r == pid)
        {
          code = WIFEXITED (wst) ? WEXITSTATUS (wst) : 98;
          break;
        }
      if (r < 0)
        break;
      if (time (NULL) > deadline)
        {
          kill (pid, SIGKILL);
          continue;
        }
      struct pollfd pf = {.fd = 0,.events = POLLIN,.revents = 0 };
      int pr = poll (&pf, 1, 200);
      if (pr > 0)
        {
          /* EOF or stray bytes both mean the control side is gone or
           * violating the protocol; neither keeps a child alive. */
          kill (pid, SIGKILL);
        }
    }

  unsigned char status = (unsigned char) code;
  ssize_t wn = write (0, &status, 1);
  (void) wn;                    /* launcher may already be gone */
  return 0;
}
