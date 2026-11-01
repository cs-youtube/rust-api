# rust-api

A small REST API in Rust (axum and serde), compared under load with
[go-api](https://github.com/cs-youtube/go-api) in the
[api-lab](https://github.com/cs-youtube/api-lab) cluster.

| Endpoint | What it does |
| --- | --- |
| `GET /items?limit=N` | Builds N items and serializes them as JSON |
| `GET /items/{id}` | One item |
| `POST /items` | Parses a JSON item, validates it and returns it |
| `GET /work?n=N` | N rounds of SHA-256 |
| `GET /metrics`, `GET /healthz` | Prometheus metrics, health |

A push to `main` runs the tests, builds the image on the lab's self-hosted runner, pushes it to
GHCR and pins its digest in [api-lab-deployment](https://github.com/cs-youtube/api-lab-deployment); Argo CD then deploys it.
Pull requests are tested on GitHub's machines, never on the lab's runner. The endpoints and metric names must
stay the same as go-api's, or the comparison stops being fair.
