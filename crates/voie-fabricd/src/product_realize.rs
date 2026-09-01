//! Daemon-owned Application, Database, and gateway manifests.
//!
//! These YAML documents are never accepted from the Fabric API. Callers
//! supply typed product fields (slug, Environment kind, port, health path,
//! Release identity). Images, RuntimeClass, ServiceAccount posture, and
//! volume layout stay deployment-owned.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::realize::{Live, WORKSPACE_SERVICE_ACCOUNT_NAME};
use crate::FabricError;

pub const APP_IMAGE: &str = "voie-app:v1";
pub const POSTGRES_IMAGE: &str = "voie-postgres:v1";
pub const GATEWAY_IMAGE: &str = "voie-gateway:v1";
pub const APP_INIT: &str = "/bin/voie-app-init";
pub const KIND_WORKSPACE: &str = "workspace";
pub const KIND_APPLICATION: &str = "application";
pub const KIND_POSTGRES: &str = "postgres";
pub const KIND_GATEWAY: &str = "gateway";
pub const KIND_EGRESS: &str = "egress";
pub const EGRESS_PORT: u16 = 8080;
pub const EGRESS_SERVICE_NAME: &str = "voie-egress";
pub const EGRESS_LISTEN: &str = "http://voie-egress:8080";
/// Host port on the Fabric node. Public Caddy reverse-proxies `*.dev`/`*.prod`
/// here over Headscale (`http://baremetal-1:8082`).
pub const GATEWAY_HOST_PORT: u16 = 8082;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppIntent {
    pub deployment_id: String,
    pub release_id: String,
    pub slug: String,
    pub kind: String,
    pub port: u16,
    pub health_path: String,
    pub run_argv: Vec<String>,
    pub cpu_millis: u32,
    pub memory_mb: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseIntent {
    pub database_id: String,
    pub slug: String,
    pub kind: String,
}

/// ClusterIP Service DNS name owned by Fabric, never a user Caddy fragment.
pub fn app_service_name(slug: &str, kind: &str) -> String {
    format!("app-{slug}-{kind}")
}

pub fn app_pod_name(deployment_id: &str) -> String {
    format!("voie-app-{}", compact_id(deployment_id))
}

pub fn postgres_service_name(database_id: &str) -> String {
    format!("pg-{}", compact_id(database_id))
}

pub fn postgres_pod_name(database_id: &str) -> String {
    format!("voie-pg-{}", compact_id(database_id))
}

pub fn postgres_restore_pod_name(operation_id: &str) -> String {
    format!("voie-pg-rst-{}", compact_id(operation_id))
}

pub fn postgres_restore_volume_name(operation_id: &str) -> String {
    format!("voie-pgdata-rst-{}", compact_id(operation_id))
}

/// Maps a live postgres LV name back to the Kubernetes PVC/PV name.
pub fn postgres_pvc_for_lv(lv_name: &str, database_id: &str) -> String {
    if let Some(compact) = lv_name.strip_prefix("rst") {
        format!("voie-pgdata-rst-{compact}")
    } else {
        postgres_volume_name(database_id)
    }
}

pub fn postgres_pod_for_lv(lv_name: &str, database_id: &str) -> String {
    if let Some(compact) = lv_name.strip_prefix("rst") {
        format!("voie-pg-rst-{compact}")
    } else {
        postgres_pod_name(database_id)
    }
}

/// Render the live postgres Pod for a claimed LV after reboot. A restore
/// LV keeps the restore Pod name and generation so the existing Service
/// selector still matches. `operation_id` is the archive/restore UUID
/// when known; the compact LV suffix is only a last-resort name match.
pub fn postgres_runtime_pod_yaml(
    live: &Live,
    database_id: &str,
    lv_name: &str,
    operation_id: Option<&str>,
    slug: &str,
    kind: &str,
) -> String {
    let intent = DatabaseIntent {
        database_id: database_id.to_owned(),
        slug: slug.to_owned(),
        kind: if kind == "prod" {
            "prod".to_owned()
        } else {
            "dev".to_owned()
        },
    };
    let pvc = postgres_pvc_for_lv(lv_name, database_id);
    if lv_name.starts_with("rst") {
        let generation = operation_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| lv_name.trim_start_matches("rst").to_owned());
        postgres_restore_pod_yaml(live, &intent, &pvc, &generation)
    } else {
        postgres_pod_yaml(live, &intent, &pvc, database_id)
    }
}

pub fn release_volume_name(release_id: &str) -> String {
    format!("voie-rel-{}", compact_id(release_id))
}

/// Per-Deployment copy of an immutable Release. Firecracker RWO cannot
/// attach the same Deployment drive to preview and production at once.
pub fn deployment_volume_name(deployment_id: &str) -> String {
    format!("voie-dep-{}", compact_id(deployment_id))
}

pub fn postgres_volume_name(database_id: &str) -> String {
    format!("voie-pgdata-{}", compact_id(database_id))
}

pub fn gateway_pod_name() -> String {
    "voie-gateway".to_owned()
}

pub fn egress_pod_name() -> String {
    "voie-egress".to_owned()
}

pub fn compact_id(value: &str) -> String {
    value.chars().filter(|ch| *ch != '-').collect()
}

pub fn verify_artifact_hash(bytes: &[u8], expected_hex: &str) -> Result<[u8; 32], FabricError> {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    if actual != expected_hex.to_ascii_lowercase() {
        return Err(FabricError::Realize(
            "release artifact hash did not match the immutable digest".into(),
        ));
    }
    Ok(digest)
}

/// Block PV for one Deployment's private copy of a Release. Preview and
/// production attach distinct drives of the same artifact hash.
pub fn deployment_pv_yaml(
    live: &Live,
    deployment_id: &str,
    device: &str,
    slug: Option<&str>,
) -> String {
    local_pv_yaml(
        live,
        &deployment_volume_name(deployment_id),
        "deployment",
        "io.voie/deployment",
        deployment_id,
        device,
        slug,
        "Block",
        crate::k8s_quantity(live.storage().deployment_bytes),
    )
}

pub fn deployment_pvc_yaml(live: &Live, deployment_id: &str, slug: Option<&str>) -> String {
    local_pvc_yaml(
        live,
        &deployment_volume_name(deployment_id),
        "deployment",
        "io.voie/deployment",
        deployment_id,
        slug,
        "Block",
        crate::k8s_quantity(live.storage().deployment_bytes),
    )
}

/// Dedicated Database data volume. Firecracker attaches this as `/dev/pgdata`;
/// a host directory is not a guest drive.
pub fn postgres_pv_yaml(
    live: &Live,
    database_id: &str,
    device: &str,
    slug: Option<&str>,
    bytes: u64,
) -> String {
    local_pv_yaml(
        live,
        &postgres_volume_name(database_id),
        "postgres",
        "io.voie/database",
        database_id,
        device,
        slug,
        "Block",
        crate::k8s_quantity(bytes),
    )
}

pub fn postgres_pvc_yaml(live: &Live, database_id: &str, slug: Option<&str>, bytes: u64) -> String {
    local_pvc_yaml(
        live,
        &postgres_volume_name(database_id),
        "postgres",
        "io.voie/database",
        database_id,
        slug,
        "Block",
        crate::k8s_quantity(bytes),
    )
}

pub fn postgres_restore_pv_yaml(
    live: &Live,
    database_id: &str,
    operation_id: &str,
    device: &str,
    slug: Option<&str>,
    bytes: u64,
) -> String {
    local_pv_yaml(
        live,
        &postgres_restore_volume_name(operation_id),
        "postgres-restore",
        "io.voie/restore",
        database_id,
        device,
        slug,
        "Block",
        crate::k8s_quantity(bytes),
    )
}

pub fn postgres_restore_pvc_yaml(
    live: &Live,
    database_id: &str,
    operation_id: &str,
    slug: Option<&str>,
    bytes: u64,
) -> String {
    local_pvc_yaml(
        live,
        &postgres_restore_volume_name(operation_id),
        "postgres-restore",
        "io.voie/restore",
        database_id,
        slug,
        "Block",
        crate::k8s_quantity(bytes),
    )
}

fn extra_slug_label(slug: Option<&str>) -> String {
    match slug {
        Some(slug)
            if !slug.is_empty()
                && !slug.contains('\n')
                && !slug.contains('"')
                && !slug.contains('\\') =>
        {
            format!("    io.voie/slug: \"{slug}\"\n")
        }
        _ => String::new(),
    }
}

fn local_pv_yaml(
    live: &Live,
    name: &str,
    kind: &str,
    identity_key: &str,
    identity: &str,
    device: &str,
    slug: Option<&str>,
    volume_mode: &str,
    size: String,
) -> String {
    format!(
        "apiVersion: v1
kind: PersistentVolume
metadata:
  name: {name}
  labels:
    io.voie/managed: \"true\"
    io.voie/kind: \"{kind}\"
    {identity_key}: \"{identity}\"
{slug_label}spec:
  capacity:
    storage: {size}
  volumeMode: {volume_mode}
  accessModes:
    - ReadWriteOnce
  persistentVolumeReclaimPolicy: Retain
  storageClassName: {sc}
  local:
    path: {device}
  nodeAffinity:
    required:
      nodeSelectorTerms:
        - matchExpressions:
            - key: kubernetes.io/hostname
              operator: In
              values:
                - {node}
",
        sc = live.storage_class(),
        node = live.node_name(),
        size = size,
        slug_label = extra_slug_label(slug),
        volume_mode = volume_mode,
    )
}

fn local_pvc_yaml(
    live: &Live,
    name: &str,
    kind: &str,
    identity_key: &str,
    identity: &str,
    slug: Option<&str>,
    volume_mode: &str,
    size: String,
) -> String {
    format!(
        "apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: {name}
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
    io.voie/kind: \"{kind}\"
    {identity_key}: \"{identity}\"
{slug_label}spec:
  accessModes:
    - ReadWriteOnce
  volumeMode: {volume_mode}
  storageClassName: {sc}
  volumeName: {name}
  resources:
    requests:
      storage: {size}
",
        ns = live.namespace(),
        sc = live.storage_class(),
        slug_label = extra_slug_label(slug),
        volume_mode = volume_mode,
    )
}

/// Application candidate Pod. Readiness is in-guest wget so kubelet does
/// not need NetworkPolicy access. There is no HTTP liveness probe: healthz
/// 503 before migrate must not kill the container. `restartPolicy: Always`
/// restarts a crashed process; cutover waits Ready.
pub fn app_pod_yaml(
    live: &Live,
    intent: &AppIntent,
    pvc_name: &str,
    env_secret: Option<&str>,
) -> Result<String, FabricError> {
    validate_slug_kind(intent)?;
    if intent.run_argv.is_empty() {
        return Err(FabricError::Config("application run argv is required"));
    }
    if intent.port == 0 {
        return Err(FabricError::Config("application port is required"));
    }
    if !intent.health_path.starts_with('/') || intent.health_path.contains('\n') {
        return Err(FabricError::Config("application health path is invalid"));
    }
    let args = render_argv(&intent.run_argv)?;
    let pod = app_pod_name(&intent.deployment_id);
    let memory = format!("{}Mi", intent.memory_mb.max(128));
    let cpu = format!("{}m", intent.cpu_millis.max(100));
    let env_from = match env_secret {
        Some(name) => {
            if name.is_empty()
                || name.len() > 63
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            {
                return Err(FabricError::Config("application env secret is invalid"));
            }
            format!(
                "
      envFrom:
        - secretRef:
            name: {name}"
            )
        }
        None => String::new(),
    };
    Ok(format!(
        "apiVersion: v1
kind: Pod
metadata:
  name: {pod}
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
    io.voie/kind: \"{kind_app}\"
    io.voie/slug: \"{slug}\"
    io.voie/environment: \"{env}\"
    io.voie/deployment: \"{deployment}\"
    io.voie/release: \"{release}\"
spec:
  restartPolicy: Always
  terminationGracePeriodSeconds: 5
  runtimeClassName: {runtime}
  nodeName: {node}
  serviceAccountName: {sa}
  automountServiceAccountToken: false
  enableServiceLinks: false
  initContainers:
    - name: mount-release
      image: {image}
      imagePullPolicy: Never
      securityContext:
        privileged: true
      command:
        - /bin/busybox
        - sh
        - -c
        - |
          mkdir -p /app /stage
          if [ ! -b /dev/app ]; then
            echo 'voie-app: /dev/app is missing' >&2
            exit 2
          fi
          if ! mount -t ext4 -o ro,noload /dev/app /app; then
            echo 'voie-app: /app did not mount as ext4' >&2
            exit 2
          fi
          cp -a /app/. /stage/
          chmod -R a+rX /stage
      volumeMounts:
        - name: app-root
          mountPath: /stage
      volumeDevices:
        - name: app
          devicePath: /dev/app
  containers:
    - name: app
      image: {image}
      imagePullPolicy: Never
      securityContext:
        privileged: false
        allowPrivilegeEscalation: false
        runAsNonRoot: true
        runAsUser: 65534
        runAsGroup: 65534
        capabilities:
          drop:
            - ALL
      command:
        - {init}
      args:{args}
      ports:
        - name: http
          containerPort: {port}
      readinessProbe:
        exec:
          command: [\"/bin/busybox\", \"sh\", \"-c\", \"HTTP_PROXY= HTTPS_PROXY= http_proxy= https_proxy= exec /bin/wget -q -O /dev/null http://127.0.0.1:{port}{health}\"]
        initialDelaySeconds: 1
        periodSeconds: 2
        timeoutSeconds: 5
        failureThreshold: 15
      resources:
        requests:
          cpu: {cpu}
          memory: {memory}
        limits:
          cpu: {cpu}
          memory: {memory}{env_from}
      volumeMounts:
        - name: app-root
          mountPath: /app
          readOnly: true
        - name: tmp
          mountPath: /tmp
  volumes:
    - name: app
      persistentVolumeClaim:
        claimName: {pvc}
    - name: app-root
      emptyDir: {{}}
    - name: tmp
      emptyDir: {{}}
",
        ns = live.namespace(),
        kind_app = KIND_APPLICATION,
        slug = intent.slug,
        env = intent.kind,
        deployment = intent.deployment_id,
        release = intent.release_id,
        runtime = live.runtime_class(),
        node = live.node_name(),
        sa = WORKSPACE_SERVICE_ACCOUNT_NAME,
        image = APP_IMAGE,
        init = APP_INIT,
        port = intent.port,
        health = intent.health_path,
        pvc = pvc_name,
    ))
}

pub fn app_service_yaml(live: &Live, intent: &AppIntent) -> Result<String, FabricError> {
    validate_slug_kind(intent)?;
    let name = app_service_name(&intent.slug, &intent.kind);
    Ok(format!(
        "apiVersion: v1
kind: Service
metadata:
  name: {name}
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
    io.voie/kind: \"{kind_app}\"
    io.voie/slug: \"{slug}\"
    io.voie/environment: \"{env}\"
spec:
  type: ClusterIP
  selector:
    io.voie/deployment: \"{deployment}\"
  ports:
    - name: http
      port: {port}
      targetPort: {port}
",
        ns = live.namespace(),
        kind_app = KIND_APPLICATION,
        slug = intent.slug,
        env = intent.kind,
        deployment = intent.deployment_id,
        port = intent.port,
    ))
}

/// Activating a candidate switches the Environment Service selector onto the
/// new Deployment identity. The previous Pod stays until cutover succeeds.
pub fn app_service_selector_yaml(
    live: &Live,
    slug: &str,
    kind: &str,
    deployment_id: &str,
    port: u16,
) -> Result<String, FabricError> {
    app_service_yaml(
        live,
        &AppIntent {
            deployment_id: deployment_id.to_owned(),
            release_id: String::new(),
            slug: slug.to_owned(),
            kind: kind.to_owned(),
            port,
            health_path: "/healthz".into(),
            run_argv: vec!["true".into()],
            cpu_millis: 100,
            memory_mb: 128,
        },
    )
}

pub fn postgres_secret_name(database_id: &str) -> String {
    format!("voie-pgcred-{}", compact_id(database_id))
}

pub fn app_env_secret_name(deployment_id: &str) -> String {
    format!("voie-appenv-{}", compact_id(deployment_id))
}

pub fn postgres_pod_yaml(
    live: &Live,
    intent: &DatabaseIntent,
    pvc_name: &str,
    generation: &str,
) -> String {
    let pod = postgres_pod_name(&intent.database_id);
    let secret = postgres_secret_name(&intent.database_id);
    format!(
        "apiVersion: v1
kind: Pod
metadata:
  name: {pod}
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
    io.voie/kind: \"{kind_pg}\"
    io.voie/database: \"{database}\"
    io.voie/database-generation: \"{generation}\"
    io.voie/slug: \"{slug}\"
    io.voie/environment: \"{env}\"
spec:
  restartPolicy: Always
  terminationGracePeriodSeconds: 10
  runtimeClassName: {runtime}
  nodeName: {node}
  serviceAccountName: {sa}
  automountServiceAccountToken: false
  enableServiceLinks: false
  containers:
    - name: postgres
      image: {image}
      imagePullPolicy: Never
      securityContext:
        privileged: true
      command:
        - /bin/busybox
        - sh
        - -c
        - |
          mkdir -p /var/lib/postgresql/data
          if [ ! -b /dev/pgdata ]; then
            echo 'voie-postgres: /dev/pgdata is missing' >&2
            exit 1
          fi
          if ! mount -t ext4 /dev/pgdata /var/lib/postgresql/data; then
            echo 'voie-postgres: data volume did not mount as ext4' >&2
            exit 1
          fi
          exec /bin/voie-postgres-init
      ports:
        - name: postgres
          containerPort: 5432
      readinessProbe:
        exec:
          command: [\"/bin/pg_isready\", \"-U\", \"app\", \"-h\", \"127.0.0.1\"]
        periodSeconds: 2
        timeoutSeconds: 5
        failureThreshold: 30
      volumeMounts:
        - name: credential
          mountPath: /run/voie
          readOnly: true
      volumeDevices:
        - name: data
          devicePath: /dev/pgdata
  volumes:
    - name: data
      persistentVolumeClaim:
        claimName: {pvc}
    - name: credential
      secret:
        secretName: {secret}
",
        ns = live.namespace(),
        kind_pg = KIND_POSTGRES,
        database = intent.database_id,
        generation = generation,
        slug = intent.slug,
        env = intent.kind,
        runtime = live.runtime_class(),
        node = live.node_name(),
        sa = WORKSPACE_SERVICE_ACCOUNT_NAME,
        image = POSTGRES_IMAGE,
        pvc = pvc_name,
    )
}

pub fn postgres_restore_pod_yaml(
    live: &Live,
    intent: &DatabaseIntent,
    pvc_name: &str,
    operation_id: &str,
) -> String {
    let pod = postgres_restore_pod_name(operation_id);
    let secret = postgres_secret_name(&intent.database_id);
    format!(
        "apiVersion: v1
kind: Pod
metadata:
  name: {pod}
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
    io.voie/kind: \"{kind_pg}\"
    io.voie/database: \"{database}\"
    io.voie/database-generation: \"{generation}\"
    io.voie/slug: \"{slug}\"
    io.voie/environment: \"{env}\"
spec:
  restartPolicy: Always
  terminationGracePeriodSeconds: 10
  runtimeClassName: {runtime}
  nodeName: {node}
  serviceAccountName: {sa}
  automountServiceAccountToken: false
  enableServiceLinks: false
  containers:
    - name: postgres
      image: {image}
      imagePullPolicy: Never
      securityContext:
        privileged: true
      command:
        - /bin/busybox
        - sh
        - -c
        - |
          mkdir -p /var/lib/postgresql/data
          if [ ! -b /dev/pgdata ]; then
            echo 'voie-postgres: /dev/pgdata is missing' >&2
            exit 1
          fi
          if ! mount -t ext4 /dev/pgdata /var/lib/postgresql/data; then
            echo 'voie-postgres: data volume did not mount as ext4' >&2
            exit 1
          fi
          exec /bin/voie-postgres-init
      ports:
        - name: postgres
          containerPort: 5432
      readinessProbe:
        exec:
          command: [\"/bin/pg_isready\", \"-U\", \"app\", \"-h\", \"127.0.0.1\"]
        periodSeconds: 2
        timeoutSeconds: 5
        failureThreshold: 30
      volumeMounts:
        - name: credential
          mountPath: /run/voie
          readOnly: true
      volumeDevices:
        - name: data
          devicePath: /dev/pgdata
  volumes:
    - name: data
      persistentVolumeClaim:
        claimName: {pvc}
    - name: credential
      secret:
        secretName: {secret}
",
        ns = live.namespace(),
        kind_pg = KIND_POSTGRES,
        database = intent.database_id,
        generation = operation_id,
        slug = intent.slug,
        env = intent.kind,
        runtime = live.runtime_class(),
        node = live.node_name(),
        sa = WORKSPACE_SERVICE_ACCOUNT_NAME,
        image = POSTGRES_IMAGE,
        pvc = pvc_name,
    )
}

pub fn postgres_service_yaml(live: &Live, intent: &DatabaseIntent, generation: &str) -> String {
    let name = postgres_service_name(&intent.database_id);
    format!(
        "apiVersion: v1
kind: Service
metadata:
  name: {name}
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
    io.voie/kind: \"{kind_pg}\"
    io.voie/database: \"{database}\"
spec:
  type: ClusterIP
  selector:
    io.voie/database: \"{database}\"
    io.voie/database-generation: \"{generation}\"
  ports:
    - name: postgres
      port: 5432
      targetPort: 5432
",
        ns = live.namespace(),
        kind_pg = KIND_POSTGRES,
        database = intent.database_id,
        generation = generation,
    )
}

pub fn postgres_network_policy_name(database_id: &str) -> String {
    format!("voie-pgnet-{}", compact_id(database_id))
}

/// Dedicated Database: only the matching Application slug and Environment
/// may connect. Dev cannot reach prod, and another Application cannot reach
/// this instance. Workspace pods stay on the workspace policy.
pub fn postgres_network_policy_yaml(
    live: &Live,
    intent: &DatabaseIntent,
) -> Result<String, FabricError> {
    if intent.slug.is_empty()
        || intent.slug.contains('.')
        || intent.slug.contains('/')
        || intent.slug.contains('\n')
        || intent.slug.contains('"')
    {
        return Err(FabricError::Config(
            "database slug is not an Application slug",
        ));
    }
    if intent.kind != "dev" && intent.kind != "prod" {
        return Err(FabricError::Config("database kind must be dev or prod"));
    }
    if intent.database_id.is_empty() || intent.database_id.contains('\n') {
        return Err(FabricError::Config("database id is invalid"));
    }
    let name = postgres_network_policy_name(&intent.database_id);
    Ok(format!(
        "apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: {name}
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
    io.voie/kind: \"{kind_pg}\"
    io.voie/database: \"{database}\"
    io.voie/slug: \"{slug}\"
    io.voie/environment: \"{env}\"
spec:
  podSelector:
    matchLabels:
      io.voie/database: \"{database}\"
  policyTypes:
    - Ingress
    - Egress
  ingress:
    - from:
        - podSelector:
            matchLabels:
              io.voie/kind: \"{kind_app}\"
              io.voie/slug: \"{slug}\"
              io.voie/environment: \"{env}\"
      ports:
        - protocol: TCP
          port: 5432
  egress: []
",
        ns = live.namespace(),
        kind_pg = KIND_POSTGRES,
        kind_app = KIND_APPLICATION,
        database = intent.database_id,
        slug = intent.slug,
        env = intent.kind,
        name = name,
    ))
}

/// Cluster-network Caddy with hostPort 8082. hostNetwork is forbidden:
/// Application NetworkPolicy admits only `io.voie/kind=gateway`, and
/// Cilium would treat a host-net guest as reserved:host.
pub fn gateway_pod_yaml(live: &Live) -> String {
    let pod = gateway_pod_name();
    format!(
        "apiVersion: v1
kind: Pod
metadata:
  name: {pod}
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
    io.voie/kind: \"{kind_gw}\"
spec:
  restartPolicy: Always
  terminationGracePeriodSeconds: 5
  nodeName: {node}
  serviceAccountName: {sa}
  automountServiceAccountToken: false
  enableServiceLinks: false
  containers:
    - name: gateway
      image: {image}
      imagePullPolicy: Never
      ports:
        - name: http
          containerPort: {host_port}
          hostPort: {host_port}
      readinessProbe:
        exec:
          command:
            - /bin/caddy
            - version
        initialDelaySeconds: 1
        periodSeconds: 5
        timeoutSeconds: 2
        failureThreshold: 6
      resources:
        requests:
          cpu: 50m
          memory: 64Mi
        limits:
          cpu: 500m
          memory: 256Mi
      volumeMounts:
        - name: caddyfile
          mountPath: /etc/caddy
          readOnly: true
  volumes:
    - name: caddyfile
      configMap:
        name: voie-gateway-caddy
",
        ns = live.namespace(),
        kind_gw = KIND_GATEWAY,
        node = live.node_name(),
        sa = WORKSPACE_SERVICE_ACCOUNT_NAME,
        image = GATEWAY_IMAGE,
        host_port = GATEWAY_HOST_PORT,
    )
}

pub fn gateway_service_yaml(live: &Live) -> String {
    format!(
        "apiVersion: v1
kind: Service
metadata:
  name: voie-gateway
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
    io.voie/kind: \"{kind_gw}\"
spec:
  type: ClusterIP
  selector:
    io.voie/kind: \"{kind_gw}\"
  ports:
    - name: http
      port: 80
      targetPort: {host_port}
",
        ns = live.namespace(),
        kind_gw = KIND_GATEWAY,
        host_port = GATEWAY_HOST_PORT,
    )
}

/// Namespace default-deny selects every pod. The gateway is the Fabric HTTP
/// edge: allow host/kubelet probes and Application reverse-proxy egress.
pub fn gateway_network_policy_yaml(live: &Live) -> String {
    format!(
        "apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: voie-gateway
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
    io.voie/kind: \"{kind_gw}\"
spec:
  podSelector:
    matchLabels:
      io.voie/kind: \"{kind_gw}\"
  policyTypes:
    - Ingress
    - Egress
  ingress:
    - ports:
        - protocol: TCP
          port: {port}
  egress:
    - to:
        - namespaceSelector:
            matchLabels:
              kubernetes.io/metadata.name: kube-system
      ports:
        - protocol: UDP
          port: 53
        - protocol: TCP
          port: 53
    - to:
        - podSelector:
            matchLabels:
              io.voie/kind: \"{kind_app}\"
",
        ns = live.namespace(),
        kind_gw = KIND_GATEWAY,
        kind_app = KIND_APPLICATION,
        port = GATEWAY_HOST_PORT,
    )
}

/// Cilium treats kubelet probes and hostPort DNAT as reserved:host.
/// Kubernetes NetworkPolicy "allow port 8082" does not admit that identity,
/// so the in-cluster gateway never becomes Ready. This platform CNP is the
/// Fabric edge allow-list, not a user policy language.
pub fn gateway_host_policy_yaml(live: &Live) -> String {
    format!(
        "apiVersion: cilium.io/v2
kind: CiliumNetworkPolicy
metadata:
  name: voie-gateway-host
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
    io.voie/kind: \"{kind_gw}\"
spec:
  endpointSelector:
    matchLabels:
      io.voie/kind: \"{kind_gw}\"
  ingress:
    - fromEntities:
        - host
        - remote-node
        - world
        - cluster
      toPorts:
        - ports:
            - port: \"{port}\"
              protocol: TCP
",
        ns = live.namespace(),
        kind_gw = KIND_GATEWAY,
        port = GATEWAY_HOST_PORT,
    )
}

/// Daemon-owned ConfigMap. The Caddyfile is generated from slug, Environment
/// kind, and Fabric Service names. Users cannot supply fragments.
pub fn gateway_caddy_configmap_yaml(live: &Live, caddyfile: &str) -> Result<String, FabricError> {
    if caddyfile.contains("\n---") || caddyfile.contains("hostPath") {
        return Err(FabricError::Config(
            "generated gateway Caddyfile is not a platform route map",
        ));
    }
    let mut body = String::new();
    for line in caddyfile.lines() {
        body.push_str("    ");
        body.push_str(line);
        body.push('\n');
    }
    Ok(format!(
        "apiVersion: v1
kind: ConfigMap
metadata:
  name: voie-gateway-caddy
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
    io.voie/kind: \"{kind_gw}\"
data:
  Caddyfile: |
{body}",
        ns = live.namespace(),
        kind_gw = KIND_GATEWAY,
    ))
}

/// Platform Application egress proxy. Same gateway image, different argv;
/// Applications may CONNECT here. The proxy Pod, not the Application Pod,
/// may use deployment-approved CIDRs.
pub fn egress_pod_yaml(live: &Live) -> String {
    let pod = egress_pod_name();
    format!(
        "apiVersion: v1
kind: Pod
metadata:
  name: {pod}
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
    io.voie/kind: \"{kind}\"
spec:
  restartPolicy: Always
  terminationGracePeriodSeconds: 5
  nodeName: {node}
  serviceAccountName: {sa}
  automountServiceAccountToken: false
  enableServiceLinks: false
  containers:
    - name: egress
      image: {image}
      imagePullPolicy: Never
      command:
        - /bin/voie-egress
      ports:
        - name: proxy
          containerPort: {port}
      resources:
        requests:
          cpu: 50m
          memory: 64Mi
        limits:
          cpu: 500m
          memory: 256Mi
",
        ns = live.namespace(),
        kind = KIND_EGRESS,
        node = live.node_name(),
        sa = WORKSPACE_SERVICE_ACCOUNT_NAME,
        image = GATEWAY_IMAGE,
        port = EGRESS_PORT,
    )
}

pub fn egress_service_yaml(live: &Live) -> String {
    format!(
        "apiVersion: v1
kind: Service
metadata:
  name: {name}
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
    io.voie/kind: \"{kind}\"
spec:
  type: ClusterIP
  selector:
    io.voie/kind: \"{kind}\"
  ports:
    - name: proxy
      port: {port}
      targetPort: {port}
",
        name = EGRESS_SERVICE_NAME,
        ns = live.namespace(),
        kind = KIND_EGRESS,
        port = EGRESS_PORT,
    )
}

/// The proxy may resolve names and, when configured, reach the same CIDRs
/// Workspace guests already use. Application Pods never get those CIDRs.
pub fn egress_network_policy_yaml(live: &Live) -> String {
    let approved = live
        .approved_egress()
        .map(|approved| {
            let mut blocks = String::new();
            for cidr in &approved.cidrs {
                blocks.push_str(&format!("        - ipBlock:\n            cidr: {cidr}\n"));
            }
            format!(
                "    - to:\n{blocks}      ports:\n        - protocol: TCP\n          port: {}\n",
                approved.tcp_port
            )
        })
        .unwrap_or_default();
    format!(
        "apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: voie-application-egress
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
    io.voie/kind: \"{kind}\"
spec:
  podSelector:
    matchLabels:
      io.voie/kind: \"{kind}\"
  policyTypes:
    - Ingress
    - Egress
  ingress:
    - from:
        - podSelector:
            matchLabels:
              io.voie/kind: \"{kind_app}\"
      ports:
        - protocol: TCP
          port: {port}
  egress:
    - to:
        - namespaceSelector:
            matchLabels:
              kubernetes.io/metadata.name: kube-system
      ports:
        - protocol: UDP
          port: 53
        - protocol: TCP
          port: 53
{approved}",
        ns = live.namespace(),
        kind = KIND_EGRESS,
        kind_app = KIND_APPLICATION,
        port = EGRESS_PORT,
    )
}

/// Application Pods share DNS and platform-egress allow-lists. Ingress from
/// the Fabric gateway uses the declared `run.port` on the per-Deployment
/// policy so an app is not limited to the tracker demo ports.
pub fn application_network_policy_yaml(live: &Live) -> String {
    format!(
        "apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: voie-application
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
    io.voie/kind: \"{kind_app}\"
spec:
  podSelector:
    matchLabels:
      io.voie/kind: \"{kind_app}\"
  policyTypes:
    - Egress
  egress:
    - to:
        - namespaceSelector:
            matchLabels:
              kubernetes.io/metadata.name: kube-system
      ports:
        - protocol: UDP
          port: 53
        - protocol: TCP
          port: 53
    - to:
        - podSelector:
            matchLabels:
              io.voie/kind: \"{kind_egress}\"
      ports:
        - protocol: TCP
          port: {egress_port}
",
        ns = live.namespace(),
        kind_app = KIND_APPLICATION,
        kind_egress = KIND_EGRESS,
        egress_port = EGRESS_PORT,
    )
}

pub fn application_postgres_policy_name(deployment_id: &str) -> String {
    format!("voie-appnet-{}", compact_id(deployment_id))
}

/// One Deployment admits gateway traffic on its declared port and may reach
/// only the PostgreSQL instance labeled with the same Application slug and
/// Environment. Dev cannot egress to prod.
pub fn application_postgres_policy_yaml(
    live: &Live,
    intent: &AppIntent,
) -> Result<String, FabricError> {
    validate_slug_kind(intent)?;
    if intent.deployment_id.is_empty() || intent.deployment_id.contains('\n') {
        return Err(FabricError::Config("deployment id is invalid"));
    }
    let name = application_postgres_policy_name(&intent.deployment_id);
    Ok(format!(
        "apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: {name}
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
    io.voie/kind: \"{kind_app}\"
    io.voie/slug: \"{slug}\"
    io.voie/environment: \"{env}\"
    io.voie/deployment: \"{deployment}\"
spec:
  podSelector:
    matchLabels:
      io.voie/deployment: \"{deployment}\"
  policyTypes:
    - Ingress
    - Egress
  ingress:
    - from:
        - podSelector:
            matchLabels:
              io.voie/kind: \"{kind_gw}\"
      ports:
        - protocol: TCP
          port: {port}
  egress:
    - to:
        - podSelector:
            matchLabels:
              io.voie/kind: \"{kind_pg}\"
              io.voie/slug: \"{slug}\"
              io.voie/environment: \"{env}\"
      ports:
        - protocol: TCP
          port: 5432
",
        ns = live.namespace(),
        kind_app = KIND_APPLICATION,
        kind_gw = KIND_GATEWAY,
        kind_pg = KIND_POSTGRES,
        slug = intent.slug,
        env = intent.kind,
        deployment = intent.deployment_id,
        port = intent.port,
        name = name,
    ))
}

fn validate_slug_kind(intent: &AppIntent) -> Result<(), FabricError> {
    if intent.slug.is_empty()
        || intent.slug.contains('.')
        || intent.slug.contains('/')
        || intent.slug.contains('\n')
    {
        return Err(FabricError::Config("route slug is not an Application slug"));
    }
    if intent.kind != "dev" && intent.kind != "prod" {
        return Err(FabricError::Config("route kind must be dev or prod"));
    }
    Ok(())
}

fn render_argv(argv: &[String]) -> Result<String, FabricError> {
    let mut out = String::new();
    for part in argv {
        if part.is_empty()
            || part.contains('\n')
            || part.contains('\0')
            || part.contains('"')
            || part.contains('\\')
        {
            return Err(FabricError::Config(
                "application run argv contains refused characters",
            ));
        }
        out.push_str("\n        - ");
        out.push_str(part);
    }
    Ok(out)
}

/// Unpacks a hashed `tar.zst` onto a local volume. Paths that escape the
/// destination are refused. Workspace snapshots and Deployment copies
/// stream from a host file.
#[cfg(test)]
pub fn extract_artifact(bytes: &[u8], dest: &Path) -> Result<(), FabricError> {
    extract_tar(
        std::io::Cursor::new(zstd::decode_all(bytes).map_err(|error| {
            FabricError::Realize(format!("release artifact is not valid zstd: {error}"))
        })?),
        dest,
    )
}

pub fn extract_archive_file(path: &Path, dest: &Path) -> Result<(), FabricError> {
    let file = std::fs::File::open(path)
        .map_err(|error| FabricError::Realize(format!("restore archive is unreadable: {error}")))?;
    let decoder = zstd::stream::read::Decoder::new(file).map_err(|error| {
        FabricError::Realize(format!("restore archive is not valid zstd: {error}"))
    })?;
    extract_tar(decoder, dest)
}

fn extract_tar<R: std::io::Read>(reader: R, dest: &Path) -> Result<(), FabricError> {
    use std::path::Component;

    std::fs::create_dir_all(dest)
        .map_err(|error| FabricError::Realize(format!("cannot create archive volume: {error}")))?;
    let dest = dest
        .canonicalize()
        .map_err(|error| FabricError::Realize(format!("archive volume is unreadable: {error}")))?;
    let mut archive = tar::Archive::new(reader);
    for entry in archive
        .entries()
        .map_err(|error| FabricError::Realize(format!("archive tar is unreadable: {error}")))?
    {
        let mut entry = entry.map_err(|error| {
            FabricError::Realize(format!("archive tar entry is unreadable: {error}"))
        })?;
        let relative = entry
            .path()
            .map_err(|_| FabricError::Realize("archive tar path is unusable".into()))?
            .into_owned();
        if relative.is_absolute() {
            return Err(FabricError::Realize(
                "archive contained an absolute path".into(),
            ));
        }
        for component in relative.components() {
            if matches!(component, Component::ParentDir | Component::RootDir) {
                return Err(FabricError::Realize(
                    "archive path escaped the volume".into(),
                ));
            }
        }
        let target = dest.join(&relative);
        if !target.starts_with(&dest) {
            return Err(FabricError::Realize(
                "archive path escaped the volume".into(),
            ));
        }
        entry
            .unpack(&target)
            .map_err(|error| FabricError::Realize(format!("cannot unpack archive: {error}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use std::path::PathBuf;

    fn live() -> Live {
        Live::from_config(&Config {
            bind: "127.0.0.1:0".into(),
            sqlite: std::env::temp_dir().join("voie-fabricd-product-realize.sqlite"),
            node_name: "node-under-test".into(),
            namespace: "voie-workspace".into(),
            storage_class: "voie-workspace-block".into(),
            runtime_class: "voie-firecracker".into(),
            runtime_handler: "kata-fc-rs-voie".into(),
            runner_image: "voie-runner:c1".into(),
            jailer_root: std::env::temp_dir().join("voie-fabricd-product-jails"),
            vg: "voie-ws".into(),
            storage: crate::StoragePolicy::test(),
            residue_wait_secs: 120,
            runtime_class_wait_secs: 60,
            kubectl_program: "kubectl".into(),
            kubectl_prefix: vec![],
            kubeconfig: None,
            crictl_program: "crictl".into(),
            crictl_prefix: vec![],
            tls_cert: PathBuf::from("/dev/null"),
            tls_key: PathBuf::from("/dev/null"),
            tls_ca: PathBuf::from("/dev/null"),
            approved_egress: None,
            client_sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .into(),
        })
        .unwrap()
    }

    fn live_with_cidrs() -> Live {
        Live::from_config(&Config {
            bind: "127.0.0.1:0".into(),
            sqlite: std::env::temp_dir().join("voie-fabricd-product-realize-cidr.sqlite"),
            node_name: "node-under-test".into(),
            namespace: "voie-workspace".into(),
            storage_class: "voie-workspace-block".into(),
            runtime_class: "voie-firecracker".into(),
            runtime_handler: "kata-fc-rs-voie".into(),
            runner_image: "voie-runner:c1".into(),
            jailer_root: std::env::temp_dir().join("voie-fabricd-product-jails"),
            vg: "voie-ws".into(),
            storage: crate::StoragePolicy::test(),
            residue_wait_secs: 120,
            runtime_class_wait_secs: 60,
            kubectl_program: "kubectl".into(),
            kubectl_prefix: vec![],
            kubeconfig: None,
            crictl_program: "crictl".into(),
            crictl_prefix: vec![],
            tls_cert: PathBuf::from("/dev/null"),
            tls_key: PathBuf::from("/dev/null"),
            tls_ca: PathBuf::from("/dev/null"),
            approved_egress: Some(crate::ApprovedEgress {
                cidrs: vec!["203.0.113.0/24".into()],
                tcp_port: 443,
            }),
            client_sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .into(),
        })
        .unwrap()
    }

    fn intent() -> AppIntent {
        AppIntent {
            deployment_id: "11111111-1111-1111-1111-111111111111".into(),
            release_id: "22222222-2222-2222-2222-222222222222".into(),
            slug: "invoice-demo".into(),
            kind: "dev".into(),
            port: 3000,
            health_path: "/healthz".into(),
            run_argv: vec!["node".into(), "dist/server.js".into()],
            cpu_millis: 500,
            memory_mb: 512,
        }
    }

    #[test]
    fn app_pod_uses_fixed_profile_readonly_app_and_init() {
        let yaml = app_pod_yaml(&live(), &intent(), "voie-rel-abc", None).expect("renders");
        assert!(yaml.contains("image: voie-app:v1\n"), "{yaml}");
        assert!(
            yaml.contains("runtimeClassName: voie-firecracker\n"),
            "{yaml}"
        );
        assert!(yaml.contains("name: mount-release\n"), "{yaml}");
        assert!(yaml.contains("devicePath: /dev/app\n"), "{yaml}");
        assert!(
            yaml.contains("mount -t ext4 -o ro,noload /dev/app /app"),
            "{yaml}"
        );
        assert!(yaml.contains("cp -a /app/. /stage/"), "{yaml}");
        assert!(
            yaml.contains("command:\n        - /bin/voie-app-init\n"),
            "{yaml}"
        );
        assert!(yaml.contains("io.voie/kind: \"application\""), "{yaml}");
        assert!(
            yaml.contains("automountServiceAccountToken: false\n"),
            "{yaml}"
        );
        assert!(!yaml.contains("hostPath"), "{yaml}");
        assert!(!yaml.contains("voie-runner:c1"), "{yaml}");
        assert!(
            yaml.contains("privileged: true\n"),
            "Firecracker extra-drive mount stays in the init container: {yaml}"
        );
        assert!(
            yaml.contains("privileged: false\n"),
            "user code must not run privileged: {yaml}"
        );
        assert!(yaml.contains("runAsNonRoot: true\n"), "{yaml}");
        assert!(
            yaml.contains("mountPath: /app\n          readOnly: true"),
            "{yaml}"
        );
        assert!(
            !yaml.contains(
                "volumeDevices:\n        - name: app\n          devicePath: /dev/app\n  volumes:"
            ),
            "the app container must not receive the raw Release device: {yaml}"
        );
        assert!(!yaml.contains("envFrom"), "{yaml}");
        assert!(!yaml.contains("DATABASE_URL"), "{yaml}");
        assert!(!yaml.contains("postgres://"), "{yaml}");
        assert!(yaml.contains("/bin/wget"), "{yaml}");
        assert!(
            yaml.contains("HTTP_PROXY="),
            "readiness wget must not inherit the egress proxy: {yaml}"
        );
        assert!(!yaml.contains("httpGet:"), "{yaml}");
        assert!(yaml.contains("readinessProbe:"), "{yaml}");
        assert!(
            !yaml.contains("livenessProbe:"),
            "healthz 503 before migrate must not kill the Application: {yaml}"
        );
    }

    #[test]
    fn app_pod_env_from_secret_has_no_plaintext_url() {
        let yaml = app_pod_yaml(
            &live(),
            &intent(),
            "voie-rel-abc",
            Some("voie-appenv-11111111111111111111111111111111"),
        )
        .expect("renders");
        assert!(yaml.contains("envFrom:"), "{yaml}");
        assert!(
            yaml.contains("name: voie-appenv-11111111111111111111111111111111"),
            "{yaml}"
        );
        assert!(!yaml.contains("postgres://"), "{yaml}");
        assert!(!yaml.contains("POSTGRES_PASSWORD"), "{yaml}");
    }

    #[test]
    fn app_pod_applies_manifest_resource_limits() {
        let mut custom = intent();
        custom.cpu_millis = 2000;
        custom.memory_mb = 2048;
        let yaml = app_pod_yaml(&live(), &custom, "voie-rel-abc", None).expect("renders");
        assert!(yaml.contains("cpu: 2000m\n"), "{yaml}");
        assert!(yaml.contains("memory: 2048Mi\n"), "{yaml}");
        assert!(!yaml.contains("cpu: 500m\n"), "{yaml}");
    }

    #[test]
    fn app_service_is_cluster_ip_keyed_by_deployment() {
        let yaml = app_service_yaml(&live(), &intent()).expect("renders");
        assert!(yaml.contains("type: ClusterIP\n"), "{yaml}");
        assert!(yaml.contains("name: app-invoice-demo-dev\n"), "{yaml}");
        assert!(
            yaml.contains("io.voie/deployment: \"11111111-1111-1111-1111-111111111111\""),
            "{yaml}"
        );
        assert!(!yaml.contains("LoadBalancer"), "{yaml}");
        assert!(!yaml.contains("NodePort"), "{yaml}");
    }

    #[test]
    fn candidate_pod_is_not_the_environment_service() {
        let pod = app_pod_yaml(&live(), &intent(), "voie-rel-abc", None).expect("renders");
        let service = app_service_yaml(&live(), &intent()).expect("renders");
        assert!(!pod.contains("kind: Service"), "{pod}");
        assert!(service.contains("kind: Service\n"), "{service}");
        assert!(
            service.contains("name: app-invoice-demo-dev\n"),
            "{service}"
        );
    }

    #[test]
    fn postgres_uses_fixed_profile_and_dedicated_volume() {
        let db = DatabaseIntent {
            database_id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into(),
            slug: "invoice-demo".into(),
            kind: "prod".into(),
        };
        let volume = postgres_volume_name(&db.database_id);
        let pod = postgres_pod_yaml(&live(), &db, &volume, &db.database_id);
        let svc = postgres_service_yaml(&live(), &db, &db.database_id);
        let pv = postgres_pv_yaml(
            &live(),
            &db.database_id,
            "/dev/voie-ws/pgaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            Some(&db.slug),
            8 * crate::storage::GIB,
        );
        let pvc = postgres_pvc_yaml(
            &live(),
            &db.database_id,
            Some(&db.slug),
            crate::storage::DATABASE_DEV_BYTES,
        );
        assert!(pod.contains("image: voie-postgres:v1\n"), "{pod}");
        assert!(pod.contains("io.voie/kind: \"postgres\""), "{pod}");
        assert!(pod.contains("devicePath: /dev/pgdata\n"), "{pod}");
        assert!(
            pod.contains("mount -t ext4 /dev/pgdata /var/lib/postgresql/data"),
            "{pod}"
        );
        assert!(
            pod.contains("privileged: true\n"),
            "Firecracker extra-drive mount needs a privileged guest: {pod}"
        );
        assert!(!pod.contains("POSTGRES_PASSWORD"), "{pod}");
        assert!(pod.contains("/bin/voie-postgres-init"), "{pod}");
        assert!(
            pod.contains("/bin/pg_isready"),
            "Ready must use the image /bin pg_isready: {pod}"
        );
        assert!(
            pod.contains("\"-h\", \"127.0.0.1\""),
            "Ready must probe TCP localhost, not a missing unix socket: {pod}"
        );
        assert!(pod.contains("secretName: voie-pgcred-"), "{pod}");
        assert!(
            pod.contains("mountPath: /run/voie\n          readOnly: true"),
            "{pod}"
        );
        assert!(svc.contains("port: 5432\n"), "{svc}");
        assert!(
            svc.contains("io.voie/database-generation: \"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa\""),
            "{svc}"
        );
        assert!(
            pod.contains("io.voie/database-generation: \"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa\""),
            "{pod}"
        );
        assert!(!svc.contains("hostPath"), "{svc}");
        let policy = postgres_network_policy_yaml(&live(), &db).expect("postgres policy");
        assert!(
            policy.contains("name: voie-pgnet-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"),
            "{policy}"
        );
        assert!(
            policy.contains("io.voie/slug: \"invoice-demo\""),
            "{policy}"
        );
        assert!(policy.contains("io.voie/environment: \"prod\""), "{policy}");
        assert!(policy.contains("io.voie/kind: \"application\""), "{policy}");
        assert!(policy.contains("port: 5432\n"), "{policy}");
        assert!(!policy.contains("io.voie/environment: \"dev\""), "{policy}");
        assert!(!policy.contains("ipBlock"), "{policy}");
        assert!(!policy.contains("fromEntities"), "{policy}");
        assert!(!policy.contains("hostPath"), "{policy}");
        assert!(!policy.contains("caddy"), "{policy}");
        let mut other = db.clone();
        other.kind = "dev".into();
        let dev = postgres_network_policy_yaml(&live(), &other).expect("dev policy");
        assert!(dev.contains("io.voie/environment: \"dev\""), "{dev}");
        assert!(!dev.contains("io.voie/environment: \"prod\""), "{dev}");
        other.kind = "stage".into();
        assert!(postgres_network_policy_yaml(&live(), &other).is_err());
        assert!(pod.contains(&format!("claimName: {volume}")), "{pod}");
        assert!(pv.contains("accessModes:\n    - ReadWriteOnce\n"), "{pv}");
        assert!(!pv.contains("hostPath"), "{pv}");
        assert!(pv.contains("volumeMode: Block\n"), "{pv}");
        assert!(pvc.contains("volumeMode: Block\n"), "{pvc}");
        assert!(pv.contains("local:\n    path:"), "{pv}");
        assert!(pvc.contains(&format!("volumeName: {volume}")), "{pvc}");
        assert!(!pvc.contains("hostPath"), "{pvc}");
        assert!(pvc.contains("io.voie/slug: \"invoice-demo\""), "{pvc}");
        assert!(pv.contains("io.voie/slug: \"invoice-demo\""), "{pv}");
        assert!(pv.contains("io.voie/database:"), "{pv}");
    }

    #[test]
    fn database_service_selects_one_generation_only() {
        let db = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        let op = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
        let live = live();
        let volume = postgres_restore_volume_name(op);
        let pod = postgres_restore_pod_name(op);
        assert_ne!(pod, postgres_pod_name(db));
        assert_ne!(volume, postgres_volume_name(db));
        let intent = DatabaseIntent {
            database_id: db.into(),
            slug: "invoice-demo".into(),
            kind: "dev".into(),
        };
        let yaml = postgres_restore_pod_yaml(&live, &intent, &volume, op);
        assert!(yaml.contains(&format!("name: {pod}\n")), "{yaml}");
        assert!(yaml.contains("io.voie/database:"), "{yaml}");
        assert!(
            yaml.contains(&format!("io.voie/database-generation: \"{op}\"")),
            "{yaml}"
        );
        let live_pod = postgres_pod_yaml(&live, &intent, &postgres_volume_name(db), db);
        assert!(
            live_pod.contains(&format!("io.voie/database-generation: \"{db}\"")),
            "{live_pod}"
        );
        let live_service = postgres_service_yaml(&live, &intent, db);
        assert!(
            live_service.contains(&format!("io.voie/database-generation: \"{db}\"")),
            "{live_service}"
        );
        assert!(
            !live_service.contains(&format!("io.voie/database-generation: \"{op}\"")),
            "{live_service}"
        );
        let candidate_service = postgres_service_yaml(&live, &intent, op);
        assert!(
            candidate_service.contains(&format!("io.voie/database-generation: \"{op}\"")),
            "{candidate_service}"
        );
        assert_ne!(live_service, candidate_service);
    }

    #[test]
    fn postgres_runtime_pod_for_restore_lv_keeps_restore_identity() {
        let db = "59c71320-05a0-4730-8589-a40b76657f1a";
        let op = "add02a42-81b4-4853-b750-2c6ede1341ab";
        let lv = format!("rst{}", compact_id(op));
        let yaml = postgres_runtime_pod_yaml(&live(), db, &lv, Some(op), "d024probe", "dev");
        let expected_pod = postgres_pod_for_lv(&lv, db);
        assert_eq!(expected_pod, postgres_restore_pod_name(op));
        assert!(yaml.contains(&format!("name: {expected_pod}\n")), "{yaml}");
        assert!(
            yaml.contains(&format!("io.voie/database-generation: \"{op}\"")),
            "{yaml}"
        );
        assert!(yaml.contains("io.voie/slug: \"d024probe\""), "{yaml}");
        assert!(yaml.contains("io.voie/environment: \"dev\""), "{yaml}");
        assert!(
            yaml.contains(&format!("claimName: {}", postgres_pvc_for_lv(&lv, db))),
            "{yaml}"
        );
        assert!(
            yaml.contains(&format!("secretName: {}", postgres_secret_name(db))),
            "{yaml}"
        );
        let live_lv = crate::lv_name_for_postgres(db);
        let live_yaml = postgres_runtime_pod_yaml(&live(), db, &live_lv, None, "d024probe", "dev");
        assert!(
            live_yaml.contains(&format!("name: {}\n", postgres_pod_name(db))),
            "{live_yaml}"
        );
        assert!(
            !live_yaml.contains("voie-pg-rst-"),
            "a non-restore LV must not render the restore Pod: {live_yaml}"
        );
    }

    #[test]
    fn two_deployments_of_one_release_use_distinct_rwo_volumes() {
        let release = "22222222-2222-2222-2222-222222222222";
        let dep_a = "11111111-1111-1111-1111-111111111111";
        let dep_b = "33333333-3333-3333-3333-333333333333";
        let rel_vol = release_volume_name(release);
        let vol_a = deployment_volume_name(dep_a);
        let vol_b = deployment_volume_name(dep_b);
        assert_ne!(vol_a, vol_b);
        assert_ne!(vol_a, rel_vol);
        assert_ne!(vol_b, rel_vol);
        let mut intent_a = intent();
        intent_a.deployment_id = dep_a.into();
        let mut intent_b = intent();
        intent_b.deployment_id = dep_b.into();
        let pod_a = app_pod_yaml(&live(), &intent_a, &vol_a, None).expect("renders");
        let pod_b = app_pod_yaml(&live(), &intent_b, &vol_b, None).expect("renders");
        assert!(pod_a.contains(&format!("claimName: {vol_a}")), "{pod_a}");
        assert!(!pod_a.contains(&format!("claimName: {rel_vol}")), "{pod_a}");
        assert!(!pod_a.contains(&format!("claimName: {vol_b}")), "{pod_a}");
        assert!(pod_b.contains(&format!("claimName: {vol_b}")), "{pod_b}");
        assert!(!pod_b.contains(&format!("claimName: {vol_a}")), "{pod_b}");
        let pv = deployment_pv_yaml(
            &live(),
            dep_a,
            "/dev/voie-ws/dep11111111111111111111111111111111",
            Some("invoice-demo"),
        );
        let pvc = deployment_pvc_yaml(&live(), dep_a, Some("invoice-demo"));
        assert!(pv.contains(&format!("name: {vol_a}\n")), "{pv}");
        assert!(pv.contains("accessModes:\n    - ReadWriteOnce\n"), "{pv}");
        assert!(pv.contains("volumeMode: Block\n"), "{pv}");
        assert!(!pv.contains("hostPath"), "{pv}");
        assert!(pv.contains("io.voie/deployment:"), "{pv}");
        assert!(pvc.contains(&format!("volumeName: {vol_a}\n")), "{pvc}");
        assert!(pvc.contains("accessModes:\n    - ReadWriteOnce\n"), "{pvc}");
    }

    #[test]
    fn gateway_is_platform_caddy_not_a_user_workload() {
        let yaml = gateway_pod_yaml(&live());
        assert!(yaml.contains("image: voie-gateway:v1\n"), "{yaml}");
        assert!(yaml.contains("io.voie/kind: \"gateway\""), "{yaml}");
        assert!(
            yaml.contains("mountPath: /etc/caddy\n          readOnly: true\n"),
            "{yaml}"
        );
        assert!(yaml.contains("hostPort: 8082\n"), "{yaml}");
        assert!(yaml.contains("cpu: 500m\n"), "{yaml}");
        assert!(!yaml.contains("hostNetwork:"), "{yaml}");
        assert!(!yaml.contains("ClusterFirstWithHostNet"), "{yaml}");
        assert!(
            yaml.contains("/bin/caddy\n            - version\n"),
            "{yaml}"
        );
        assert!(!yaml.contains("tcpSocket:"), "{yaml}");
        assert!(!yaml.contains("runtimeClassName"), "{yaml}");
        let svc = gateway_service_yaml(&live());
        assert!(svc.contains("name: voie-gateway\n"), "{svc}");
        assert!(svc.contains("type: ClusterIP\n"), "{svc}");
        assert!(svc.contains("targetPort: 8082\n"), "{svc}");
        assert!(!svc.contains("LoadBalancer"), "{svc}");
        let host_policy = gateway_host_policy_yaml(&live());
        assert!(
            host_policy.contains("kind: CiliumNetworkPolicy\n"),
            "{host_policy}"
        );
        assert!(
            host_policy.contains("fromEntities:\n        - host\n"),
            "{host_policy}"
        );
        assert!(host_policy.contains("port: \"8082\""), "{host_policy}");
        assert!(!host_policy.contains("hostPath"), "{host_policy}");
        let gw_policy = gateway_network_policy_yaml(&live());
        assert!(
            gw_policy.contains("io.voie/kind: \"application\""),
            "{gw_policy}"
        );
        assert!(
            !gw_policy.contains("port: 3000"),
            "gateway-to-app egress must not pin the tracker demo port: {gw_policy}"
        );
        assert!(
            !gw_policy.contains("port: 80\n"),
            "gateway-to-app egress must not pin HTTP/80: {gw_policy}"
        );
        let caddyfile = crate::routes::render_map(&[], "console.test").expect("map");
        let cm = gateway_caddy_configmap_yaml(&live(), &caddyfile).expect("configmap");
        assert!(cm.contains("name: voie-gateway-caddy\n"), "{cm}");
        assert!(cm.contains("admin unix//tmp/caddy-admin.sock"), "{cm}");
        assert!(!cm.contains("admin off"), "{cm}");
        assert!(!cm.contains("hostPath"), "{cm}");
        assert!(!cm.contains("\n---"), "{cm}");
    }

    #[test]
    fn application_policy_admits_only_gateway_ingress() {
        let yaml = application_network_policy_yaml(&live());
        assert!(yaml.contains("io.voie/kind: \"application\""), "{yaml}");
        assert!(
            !yaml.contains("io.voie/kind: \"gateway\""),
            "declared-port ingress is per-Deployment, not the shared policy: {yaml}"
        );
        assert!(yaml.contains("io.voie/kind: \"egress\""), "{yaml}");
        assert!(
            !yaml.contains("io.voie/kind: \"postgres\""),
            "shared policy must not OR postgres to every Application: {yaml}"
        );
        assert!(!yaml.contains("ipBlock"), "{yaml}");
        assert!(!yaml.contains("fromEntities"), "{yaml}");
        assert!(!yaml.contains("hostPath"), "{yaml}");
        assert!(!yaml.contains("caddy"), "{yaml}");
    }

    #[test]
    fn application_postgres_egress_is_per_environment() {
        let intent = AppIntent {
            deployment_id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into(),
            release_id: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".into(),
            slug: "invoice-demo".into(),
            kind: "prod".into(),
            port: 3000,
            health_path: "/healthz".into(),
            run_argv: vec!["true".into()],
            cpu_millis: 100,
            memory_mb: 128,
        };
        let yaml = application_postgres_policy_yaml(&live(), &intent).expect("app postgres policy");
        assert!(
            yaml.contains("name: voie-appnet-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"),
            "{yaml}"
        );
        assert!(yaml.contains("io.voie/slug: \"invoice-demo\""), "{yaml}");
        assert!(yaml.contains("io.voie/environment: \"prod\""), "{yaml}");
        assert!(yaml.contains("io.voie/kind: \"postgres\""), "{yaml}");
        assert!(yaml.contains("io.voie/kind: \"gateway\""), "{yaml}");
        assert!(yaml.contains("port: 3000\n"), "{yaml}");
        let mut custom_port = intent.clone();
        custom_port.port = 8080;
        let custom =
            application_postgres_policy_yaml(&live(), &custom_port).expect("declared port");
        assert!(custom.contains("port: 8080\n"), "{custom}");
        assert!(!custom.contains("port: 3000\n"), "{custom}");
        assert!(
            yaml.contains("io.voie/deployment: \"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa\""),
            "{yaml}"
        );
        assert!(!yaml.contains("io.voie/environment: \"dev\""), "{yaml}");
        assert!(!yaml.contains("ipBlock"), "{yaml}");
        assert!(!yaml.contains("fromEntities"), "{yaml}");
        let mut other = intent.clone();
        other.kind = "dev".into();
        let dev = application_postgres_policy_yaml(&live(), &other).expect("dev app postgres");
        assert!(dev.contains("io.voie/environment: \"dev\""), "{dev}");
        assert!(!dev.contains("io.voie/environment: \"prod\""), "{dev}");
        other.kind = "stage".into();
        assert!(application_postgres_policy_yaml(&live(), &other).is_err());
    }

    #[test]
    fn egress_proxy_is_platform_owned_not_a_user_caddy_fragment() {
        let pod = egress_pod_yaml(&live());
        let svc = egress_service_yaml(&live());
        let policy = egress_network_policy_yaml(&live());
        assert!(pod.contains("image: voie-gateway:v1\n"), "{pod}");
        assert!(
            pod.contains("command:\n        - /bin/voie-egress\n"),
            "{pod}"
        );
        assert!(pod.contains("io.voie/kind: \"egress\""), "{pod}");
        assert!(!pod.contains("runtimeClassName"), "{pod}");
        assert!(!pod.contains("caddy"), "{pod}");
        assert!(pod.contains("cpu: 500m"), "{pod}");
        assert!(pod.contains("memory: 256Mi"), "{pod}");
        assert!(svc.contains("name: voie-egress\n"), "{svc}");
        assert!(svc.contains("type: ClusterIP\n"), "{svc}");
        assert!(!svc.contains("LoadBalancer"), "{svc}");
        assert!(!policy.contains("caddy"), "{policy}");
        assert!(!policy.contains("hostPath"), "{policy}");
    }

    #[test]
    fn approved_cidrs_belong_to_the_proxy_not_the_application() {
        let live = live_with_cidrs();
        let app = application_network_policy_yaml(&live);
        let proxy = egress_network_policy_yaml(&live);
        assert!(!app.contains("ipBlock"), "{app}");
        assert!(proxy.contains("ipBlock"), "{proxy}");
        assert!(proxy.contains("cidr: 203.0.113.0/24"), "{proxy}");
        assert!(proxy.contains("port: 443"), "{proxy}");
    }

    #[test]
    fn artifact_hash_mismatch_is_refused() {
        let bytes = b"tar.zst-bytes";
        let digest = Sha256::digest(bytes);
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        verify_artifact_hash(bytes, &hex).expect("matching hash");
        assert!(verify_artifact_hash(bytes, "00".repeat(32).as_str()).is_err());
    }

    #[test]
    fn extract_roundtrip_and_refuses_garbage() {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_ustar();
            header.set_path("ok.txt").unwrap();
            header.set_size(4);
            header.set_cksum();
            builder.append(&header, b"data".as_slice()).unwrap();
            builder.finish().unwrap();
        }
        let artifact = zstd::encode_all(&tar_bytes[..], 3).unwrap();
        let dest = std::env::temp_dir().join(format!(
            "voie-extract-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dest).unwrap();
        extract_artifact(&artifact, &dest).expect("extracts a normal archive");
        assert_eq!(std::fs::read(dest.join("ok.txt")).unwrap(), b"data");
        let staged = dest.join("staged.tar.zst");
        std::fs::write(&staged, &artifact).unwrap();
        let dest2 = dest.join("from-file");
        std::fs::create_dir_all(&dest2).unwrap();
        extract_archive_file(&staged, &dest2).expect("extracts from a file");
        assert_eq!(std::fs::read(dest2.join("ok.txt")).unwrap(), b"data");
        assert!(extract_artifact(b"not-zstd", &dest).is_err());
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn argv_newlines_are_refused() {
        let mut bad = intent();
        bad.run_argv = vec!["node\n-c".into()];
        assert!(app_pod_yaml(&live(), &bad, "pvc", None).is_err());
    }
}
