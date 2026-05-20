# Thumbnail Cache Fix — Two-Tier S3-Backed Cache

## Problem

The thumbnail cache is a pure in-memory `HashMap<Uuid, Bytes>`. This has two failure modes:

1. **OOM on restart under load.** The in-memory cache is empty after every restart. The
   browser requests all visible thumbnails simultaneously, each one pulling the full original
   asset from S3 and decoding it in memory. A modest library of large images easily exceeds
   the container memory limit, OOMKilling the process.

2. **Unbounded memory growth.** The HashMap has no eviction policy — it grows forever as
   assets are viewed, eventually OOMKilling the process even without a restart.

## Solution — Two-Tier Cache

```
Request for thumbnail
       │
       ▼
1. In-memory LRU cache ──hit──▶ serve (fast path, no I/O)
       │ miss
       ▼
2. S3 thumbnails/{id}.png ──hit──▶ load into LRU, serve (no re-render)
       │ miss
       ▼
3. Download original from S3, render PNG, upload to S3 thumbnails/{id}.png,
   load into LRU, serve
```

### Properties after fix
- **Restart cost**: in-memory cache is cold, but S3 already has rendered thumbnails →
  no re-rendering, no memory spike, no OOM.
- **Steady-state memory**: LRU evicts least-recently-used entries at capacity →
  bounded memory regardless of library size.
- **Eviction (DELETE /assets/:id/thumbnail)**: removes from both LRU and S3 →
  next GET re-renders from the original.
- **Asset deletion**: removing an asset also removes its S3 thumbnail and LRU entry →
  no orphans accumulate in the bucket.

## Files Changed

| File | Change |
|------|--------|
| `Cargo.toml` | Add `lru = "0.12"` dependency |
| `src/config.rs` | Add `thumbnail_cache_capacity: usize` (env `THUMBNAIL_CACHE_CAPACITY`, default 512) |
| `src/main.rs` | Change `ThumbnailCache` from `RwLock<HashMap>` to `Mutex<LruCache>`; wire capacity from config |
| `src/storage.rs` | Add `try_download` — returns `Ok(None)` on 404, `Err` on all other failures |
| `src/handlers/assets.rs` | Update `get_thumbnail`, `delete_thumbnail`, `delete_asset` |

## Cache Capacity

Default: **512 entries**. At 256 px output size, rendered PNGs are typically 20–60 KB.
512 entries ≈ 25 MB average, 30 MB peak — well within any reasonable memory budget.
Override with `THUMBNAIL_CACHE_CAPACITY` env var if needed.

## Known Limitation (future work — not in this fix)

**Thundering herd on first-time generation.** If many requests arrive simultaneously for a
thumbnail that has never been rendered (e.g., a freshly uploaded asset), all of them will
miss both cache layers and race to render and upload. The result is harmless (identical PNGs,
last write wins on S3) but wastes CPU and S3 bandwidth. A per-asset `tokio::sync::OnceCell`
or semaphore would eliminate this. Not blocking — address in a follow-up.

## Branch

`fix/thumbnail-s3-cache` in `ullav-dam-server`.
No Helm chart changes required (same S3 bucket and credentials; new `thumbnails/` prefix
is created automatically on first write).
