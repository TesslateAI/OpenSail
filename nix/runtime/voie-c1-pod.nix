# The pod that realizes checkpoint C1.
#
# Declared here rather than typed into a recipe so the exact bytes the cluster
# converges on are reviewable and content-addressed. The RuntimeClass name
# (voie-firecracker) selects the CRI handler kata-fc-rs-voie registered by
# nix/runtime/kata-runtime-rs.nix; the entrypoint is the runner image's
# /bin/voie-runner, so the container arguments are the runner invocation.
# The runner preserves argv verbatim with no implicit shell, so the demo asks
# for /bin/sh -c itself: a 30 s deadline around `printf ok && sleep 20`. The
# trailing sleep holds the Firecracker sandbox open long enough for the
# live-c1 recipe to inspect the jailed VMM's host identity before teardown;
# it contributes nothing to the log, which must still be exactly `ok`.
{
  writeText,
}:
writeText "voie-c1-pod.yaml" ''
  apiVersion: v1
  kind: Pod
  metadata:
    name: voie-c1
    labels:
      io.voie/checkpoint: "c1"
  spec:
    restartPolicy: Never
    runtimeClassName: voie-firecracker
    # No default service-account token and no kube-style service link
    # environment may reach the Firecracker guest: the guest image carries
    # no Kubernetes identity, so a projected credential would be pure leak
    # surface. Workspace pods must carry the same two fields.
    automountServiceAccountToken: false
    enableServiceLinks: false
    containers:
      - name: runner
        image: voie-runner:c1
        imagePullPolicy: Never
        # C1 is runtime-only: no workspace volume and no guest cgroup v2
        # mount. The runner still requires a writable cgroup parent for
        # exec containment; this directory is that parent inside the guest.
        env:
          - name: VOIE_EXEC_CGROUP_ROOT
            value: /tmp/voie-exec-cgroup
        args:
          - --timeout-ms
          - "30000"
          - --
          - /bin/sh
          - -c
          - printf ok && sleep 20
''
