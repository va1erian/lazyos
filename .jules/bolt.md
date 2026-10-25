## 2026-10-25 - Pre-reserving line capacity and direct push in raster row encoding
**Learning:** In image rasterization loops, using `extend_from_slice(&p[..3])` per 4-byte chunk triggers slice metadata checks and capacity validation on every pixel. Pre-reserving the line capacity and pushing RGBA channels individually enables vectorization and avoids per-pixel call overhead.
**Action:** When converting pixel formats row-by-row, reserve full row byte capacity upfront and use direct element pushes rather than small per-pixel slice extensions.
