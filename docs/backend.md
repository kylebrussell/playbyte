# Backend (Draft)

Playbyte's optional backend serves a public feed of Byte metadata and
binary assets (savestates + thumbnails). The client continues to use the
same Byte container format.

## Endpoints (planned)

- `GET /feed` → list of `ByteMetadata` objects.
- `GET /bytes/:id` → single `ByteMetadata`.
- `GET /bytes/:id/state` → raw `state.zst`.
- `GET /bytes/:id/thumbnail` → thumbnail image bytes.
- `POST /bytes` → upload a new Byte (metadata + state + thumbnail).

## Storage design (planned)

- **Postgres** for metadata (Byte records, tags, authors).
- **Object storage** (S3-compatible) for `state.zst` and `thumbnail.png`.
- Metadata stores object keys and SHA-256 hashes for integrity.

## Notes

- The backend is optional; the desktop app can run fully offline.
- ROMs are never uploaded; Bytes reference ROM hashes.
