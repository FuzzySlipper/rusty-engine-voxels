# Animated voxel quality and runtime report

Status: checked M12H corpus evidence at Rusty Engine
`1703f46f1624d32b8324f831107a068d5f66ab30`

This report compares two conversions of the same CC0 Kenney retro character. Both select the
`idle`, `run`, and `jump` clips at 6 Hz, preserve source-space motion, store 15 runtime frames from
16 sampled poses, and deduplicate those frames to 14 meshes. The checked JSON reports under
`evidence/` are the detailed source of truth; timings below are one unoptimized local observation,
not pass/fail thresholds.

## Measured cost

| Measure | 24 × 36 × 24 baseline | 96 × 144 × 96 high fidelity |
|---|---:|---:|
| Cell size | 0.125 | 0.03125 |
| Source import | 18.1 ms | 18.8 ms |
| Conversion | 824 ms | 8.95 s |
| Amortized conversion per sampled pose | 51.5 ms | 559 ms |
| Aggregate conversion voxels | 9,650 | 168,907 |
| Canonical object | 656,537 bytes | 12,758,243 bytes |
| Runtime admission and meshing | 122 ms | 2.57 s |
| Resolved cell storage | 289,888 bytes | 5,061,696 bytes |
| Unique mesh payload storage | 1,805,208 bytes | 34,541,208 bytes |
| Unique mesh faces | 15,042 | 287,842 |
| Complete retained projection JSON | 2,397,927 bytes | 54,564,714 bytes |
| Studio open control response | 23,922 bytes | 24,805 bytes |
| Packed Studio mesh resource | 1,805,056 bytes | 34,541,056 bytes |
| Rust projection CPU per frame swap | 6.71 µs | 6.33 µs |
| Incremental frame-swap JSON | 89 bytes | 89 bytes |

The 4× linear grid improves the useful visual signal substantially, but it is not an interactive
conversion default: this observation made conversion about 10.9× slower, admission/meshing about
21.1× slower, and the unique CPU mesh payload about 19.1× larger. Frame switching stays effectively
constant because the admitted object and its 14 geometries are retained; each ordinary update is a
small `setVoxelObjectFrame` selection instead of a remesh or reupload.

The complete retained-projection JSON row remains a diagnostic measurement of the compatibility
shape. Studio no longer sends it: the control response references the packed resource row. On the
high-fidelity project, the earlier expanded-stream parse proxy averaged 2,028 ms per pass; one
current observation measured the compact response at 0.207 ms in Node and 0.4 ms in Chromium. The
same Chromium run fetched the 34,541,056-byte resource in 58.9 ms and visibly opened it through the
shared renderer. See `evidence/mesh-data-plane.json` for interpretation limits.

## Pose and silhouette comparison

The quality pass samples the exact source timestamps named by each converted-frame readout. It
records source and voxel bounds, centroids, foot anchors, material slots, consecutive-pose
continuity, the loop seam, and a deterministic 32 × 32 front-projection silhouette. The silhouette
score is a structural comparison, not a perceptual art rating.

| Clip | Baseline minimum source/voxel silhouette | High-fidelity minimum source/voxel silhouette | High-fidelity loop seam, source / voxel |
|---|---:|---:|---:|
| `idle` | 0.194 | 0.921 | 1.000 / 0.998 |
| `jump` | 0.451 | 0.905 | 1.000 / 1.000 |
| `run` | 0.190 | 0.910 | 0.877 / 0.839 |

The result matches the visible experiment: the 24 × 36 × 24 character communicates animation but
is overtly blocky, while the 96 × 144 × 96 character retains much more of the sampled silhouette.
The `run` seam is intentionally not identical because `excludeLoopSeam` omits the duplicate exact
endpoint; source and voxel continuity remain close rather than pretending the last stored pose is
the first pose. The high-fidelity maximum normalized extent error is 0.0183 and maximum normalized
foot-anchor error is 0.1216 across this corpus. Every frame uses the same checked material slot.

## Runtime behavior

The baseline project saves its instance at the canonical default frame and selects clips only in a
disposable player. Its collision policy resolves that default frame once. Checked runtime and
adapter evidence proves:

- the default pose displays before playback;
- named `idle` and `run` selection, once terminal settlement, repeat wrapping, pause/resume, and
  serialized posture restoration use explicit caller time;
- close/reopen reconstructs the same project and object identities;
- missing and corrupt canonical objects are rejected through the normal project-open path;
- playback changes neither project nor object bytes; and
- the stable collision frame keeps the same voxel-data hash while visible frames change.

The exact Engine integration gate additionally drives these controls in Chromium through Studio and
the shared Three renderer. It observes the normal renderer acknowledgement and retained-frame hash;
it does not introduce a test-only rendering authority.

## Practical posture and limits

The checked baseline is appropriate for quick iteration. The high-fidelity asset is useful as an
offline quality target and proves the format/runtime can carry a materially denser result, but its
current unoptimized initial load is too expensive for casual hot reload. Products should preload or
use a deliberately chosen intermediate resolution rather than treating the schema maximum as a
budget.

Hard Engine ceilings remain much larger than this corpus: 256 durable clips, 4,096 frames per clip,
8,192 total animation frames, 16,777,216 aggregate represented voxels, a 64 MiB canonical artifact,
and 4,096 sampled poses per sampling request. Those are rejection bounds, not recommended product
targets. The measured 15-frame/14-mesh corpus is the only performance claim here.

## Known limitations

- This is one licensed humanoid corpus with one material. It does not establish quality for hard
  surface models, multiple palettes, long clips, or many simultaneous instances.
- The 6 Hz flipbook cadence is intentionally stylized. The runtime swaps complete poses and does not
  interpolate voxel positions.
- Conservative surface voxelization thickens features below one cell; the low-resolution result
  therefore has visibly quantized limbs and coarse foot placement.
- Rust projection CPU and retained payload bytes are measured directly. Chromium proves real WebGL
  frame switching and acknowledgement latency, but the current public renderer surface does not
  expose a GPU timer query or driver-reported VRAM. The report therefore does not claim isolated GPU
  milliseconds or exact VRAM.
- Schema 1 stores complete frames. A delta/reference encoding should be considered only after more
  corpora show that its complexity would materially improve practical load or memory costs.
