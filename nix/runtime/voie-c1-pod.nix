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
    # Readiness proves the block-backed workspace is really mounted inside
    # the guest (a /proc/mounts entry), not merely a directory on the rootfs
    # snapshot. Consumers must gate on the Kubernetes Ready condition, never
    # on phase Running plus ad-hoc exec checks.
    readinessProbe:
      exec:
        command:
          - /bin/sh
          - -c
          - grep -qs " /workspace " /proc/mounts
      initialDelaySeconds: 1
      periodSeconds: 2
      timeoutSeconds: 2
      failureThreshold: 30
    containers:
      - name: runner
        image: voie-runner:c1
        imagePullPolicy: Never
        args:
          - --timeout-ms
          - "30000"
          - --
          - /bin/sh
          - -c
          - printf ok && sleep 20
''
