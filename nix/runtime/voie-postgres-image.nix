# Fixed PostgreSQL guest. One dedicated instance per Application Environment.
{
  dockerTools,
  busybox,
  postgresql_17,
}:
dockerTools.buildLayeredImage {
  name = "voie-postgres";
  tag = "v1";
  contents = [
    busybox
    postgresql_17
  ];
  extraCommands = ''
    mkdir -p var/lib/postgresql tmp run/voie run/postgresql bin etc
    printf '%s\n' 'root:x:0:0:root:/root:/bin/sh' 'postgres:x:70:70:postgres:/var/lib/postgresql:/bin/sh' > etc/passwd
    printf '%s\n' 'root:x:0:' 'postgres:x:70:' > etc/group
    {
      echo '#!/bin/sh'
      echo 'set -eu'
      echo 'export PATH=${postgresql_17}/bin:/bin:/usr/bin'
      cat ${./voie-postgres-init.sh}
    } > bin/voie-postgres-init
    chmod +x bin/voie-postgres-init
    ln -sfn ${postgresql_17}/bin/pg_isready bin/pg_isready
    ln -sfn ${postgresql_17}/bin/initdb bin/initdb
    ln -sfn ${postgresql_17}/bin/postgres bin/postgres
    ln -sfn ${postgresql_17}/bin/psql bin/psql
    ln -sfn ${postgresql_17}/bin/pg_ctl bin/pg_ctl
    ln -sfn ${postgresql_17}/bin/pg_dump bin/pg_dump
    ln -sfn ${postgresql_17}/bin/pg_restore bin/pg_restore
    ln -sfn ${busybox}/bin/busybox bin/busybox
    ln -sfn busybox bin/cat
  '';
  config = {
    Entrypoint = [ "${postgresql_17}/bin/postgres" ];
    Env = [
      "PGDATA=/var/lib/postgresql/data"
    ];
  };
  meta = {
    description = "Deployment-owned voie-postgres:v1 database runtime guest";
  };
}
