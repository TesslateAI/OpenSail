# voie-cloud

VOIE Cloud Release 0 is a narrow production pivot: one trusted control plane, one portable external Firecracker fabric, disposable DSH activations, and one Web surface.

Read in this order:

1. `RELEASE.md` — current release and checkpoint truth.
2. `ARCHITECTURE.md` — accepted system boundaries and state ownership.
3. `ENGINEERING.md` — permanent development rules.
4. `AGENTS.md` — bounded agent work contract.

Local baseline:

```bash
nix develop --command just check
```

No product capability is claimed until its checkpoint records a `PASS` commit on `main`.
