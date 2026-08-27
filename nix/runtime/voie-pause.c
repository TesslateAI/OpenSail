/*
 * Minimal, faithful Kubernetes pause replacement for the pinned sandbox
 * infrastructure image.
 *
 * Contract it must honor (same as upstream pause):
 *   - runs as PID 1 of the pod sandbox and therefore OWNS orphan reaping;
 *     exiting early or leaking zombies breaks every sibling process,
 *   - blocks forever when there is no work,
 *   - exits cleanly on SIGINT/SIGTERM so the sandbox can be torn down.
 *
 * Implementation is deliberately tiny and static: one binary, no libc
 * dynamic dependencies inside the guest rootfs.
 */
#include <errno.h>
#include <signal.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile sig_atomic_t terminate_requested = 0;

static void
on_terminate (int signo)
{
  (void) signo;
  terminate_requested = 1;
}

int
main (void)
{
  struct sigaction sa;
  memset (&sa, 0, sizeof (sa));
  sa.sa_handler = on_terminate;
  sigemptyset (&sa.sa_mask);

  if (sigaction (SIGINT, &sa, NULL) < 0)
    return 1;
  if (sigaction (SIGTERM, &sa, NULL) < 0)
    return 1;

  /* Block the interesting signals so sigsuspend below can wait for them
   * atomically (no lost-wakeup race between reaping and sleeping). */
  sigset_t blocked;
  sigset_t previous;
  sigemptyset (&blocked);
  sigaddset (&blocked, SIGCHLD);
  sigaddset (&blocked, SIGINT);
  sigaddset (&blocked, SIGTERM);
  if (sigprocmask (SIG_BLOCK, &blocked, &previous) < 0)
    return 1;

  for (;;)
    {
      if (terminate_requested)
	return 0;

      /* Reap every finished child; as PID 1 we are the designated grave
       * digger of the sandbox. */
      int status;
      while (waitpid (-1, &status, WNOHANG) > 0)
	continue;

      sigsuspend (&previous);
    }
}
