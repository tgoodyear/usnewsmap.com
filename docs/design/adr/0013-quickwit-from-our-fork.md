# ADR-0013: We build Quickwit from our fork

- **Status:** Accepted
- **Date:** 2026-10
- **Amends:** ADR-0001 (Quickwit as published) and 08 §8.6 (the Quickwit image copied from Docker Hub)

## Context

Cold searches on the American Stories index (640 splits, two text fields) were 3 to 6 times slower than on the index before it (#251). In dev, on a 1% sample index (58 splits) and the searcher's production shape, the searcher's main runtime was busy 2.0 to 3.0 s per cold search reading from Blob, and most of that was TLS (`ring` big-number arithmetic was the top 60% of its profile). The same index served from an NFS share took 0.15 to 0.26 s.

Quickwit 0.9.1's source shows why. `azure_core` 0.21 builds its HTTP client with `pool_max_idle_per_host(0)`, a workaround for hyperium/hyper#2312, so every ranged GET to Blob opens a new TCP connection and does a full TLS handshake. Quickwit also builds a new Blob client and a new credential for every leaf search request, so each search parses the root store again and starts with an empty token cache. No setting turns pooling on, and Quickwit's main branch still uses the same transport.

The fix is one commit to Quickwit's Azure storage: a pooled HTTP client shared by every Blob client in the process (HTTP/1.1, 50 s idle timeout), and one token credential. In dev, on the searcher's production shape and the same 13 searches, that build was as fast as NFS or faster:

|                                      | Blob, 0.9.1 | NFS, 0.9.1     | Blob, our build |
| ------------------------------------ | ----------- | -------------- | --------------- |
| Cold median, first pass              | 0.72 s      | 0.30 s         | 0.18 s          |
| Median at 10 at once                 | 3.62 s      | 0.55 to 0.69 s | 0.55 to 0.67 s  |
| Searches/s at 10 at once             | 1.46        | 8.4 to 8.5     | 9.2 to 11.2     |
| Main runtime busy s per search, cold | 2.0 to 3.0  | 0.15 to 0.26   | 0.09 to 0.26    |
| Failed searches                      | 0           | 0              | 0               |

No search hung at 4 or 10 at once. A hang is the risk `azure_core` turned pooling off to avoid.

## Decision

- **Production runs Quickwit v0.9.1 plus that commit**, from our fork [`tgoodyear/quickwit`](https://github.com/tgoodyear/quickwit): branch `usnm/v0.9.1-pool`, tag `usnm-v0.9.1-pool1` (`5fecc2c`). The searcher sidecar, the ingest image (whose Quickwit is the release's writer) and the ingest job's init container all use it.
- **`Dockerfile.quickwit-patched` builds it** from the fork at a pinned commit, with the stages, base images, packages and features of Quickwit's own v0.9.1 Dockerfile, so the image is a drop-in for `quickwit/quickwit:v0.9.1`.
- **Each environment builds it in its own registry** with ACR Tasks (`scripts/build-quickwit.sh --env <env>`, about an hour), or imports another environment's build by digest. `infra/quickwit-image.json` records each environment's digest; the Bicep and CI's publish read it there. An environment it doesn't list runs upstream v0.9.1, copied in by CI as before, so a new environment still deploys before anyone builds anything.
- `QW_AZURE_POOL=false` on a container restores upstream's client without a new image.

## Alternatives

| Option                                      | Why not                                                                                                                                                                  |
| ------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Serve the index from an NFS share           | About $120 a month for the 1.16 TB share and a copy step in every release, for about the same speed                                                                      |
| An older Quickwit (pre-0.9 nightly)         | Measured about 3 times slower than 0.9.1 on the same splits, with about 3 times the runtime work                                                                         |
| A TLS-pooling proxy or an S3 gateway        | The proxy needs an endpoint override only in Quickwit's unreleased main, and sends the token in plain text over localhost; the gateway needs a third-party Azure backend |
| Wait for upstream                           | Quickwit's main has the same transport                                                                                                                                   |
| Build once and publish to a public registry | A new public artifact to maintain; per-environment builds keep images private (08 §8.6)                                                                                  |

## Consequences

- **We maintain a Quickwit build.** A Quickwit upgrade means rebasing the commit onto the new tag on the fork, a new `usnm-*` tag, and a new build in each environment ([operations](../../operations.md#the-quickwit-image)).
- **A build takes about an hour** on ACR Tasks' default agent, by hand, not in CI. Two builds of the same commit get different digests, so the file records one per environment.
- **CI's Quickwit tests stay on upstream's v0.9.1 binary.** Our commit changes only the Blob client, which the parity and pipeline tests don't reach; they index and search local files.
- **The writer runs the same build.** The dev measurements were of searches; the ingest job's uploads were not measured before the switch.
