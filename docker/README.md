# Container images

The Dockerfiles for the images the service runs in Azure. Every build uses the repository root as its context (`COPY` paths are relative to the root, not to this directory), so name the file with `-f`:

```sh
docker build -f docker/Dockerfile.api .
```

| File                          | Builds                                                                                                                 | Built by                                                                                                                                                       |
| ----------------------------- | ---------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Dockerfile.api`              | `usnewsmap-api`: the API binary and the built web app it serves                                                        | the `image` (check build) and `publish` (push on `main`) jobs in `.github/workflows/ci.yml` and `scripts/azure/stand-up.sh`                                    |
| `Dockerfile.ingest`           | `usnewsmap-ingest`: `usnm-ingest` and `usnm-qwcluster` on the pinned Quickwit image                                    | the same CI jobs and `scripts/azure/stand-up.sh`; the manual builds in [operations](../docs/operations/README.md)                                              |
| `Dockerfile.ja-ocr`           | `usnewsmap-ja-ocr`: the Japanese OCR job (`ja-ocr/`)                                                                   | the same CI jobs, `scripts/azure/stand-up.sh` and, on an OCR node, `scripts/ocr-node/ja-ocr-onprem.sh`                                                         |
| `Dockerfile.quickwit-patched` | `quickwit/quickwit-usnm`: Quickwit built from our fork ([ADR-0013](../docs/design/adr/0013-quickwit-from-our-fork.md)) | `scripts/azure/build-quickwit.sh`, in the environment's registry with ACR Tasks. The context is this one file: the Dockerfile fetches the source from the fork |

`.dockerignore` stays at the repository root, where the build context is.
