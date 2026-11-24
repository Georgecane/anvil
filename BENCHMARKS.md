# Anvil performance benchmarks

These benchmarks establish a local baseline for the Deque Tree, registry, mutation recording, and Stylo invalidation paths. They are diagnostic measurements, not CI performance gates.

## Running

~~~bash
cargo bench --bench baseline

# Approximate resident-memory delta for either fixture:
cargo bench --bench memory -- tree 8191
cargo bench --bench memory -- ui 8191
~~~

The memory benchmark accepts tree or ui followed by the item count. Run each size in a fresh process when comparing RSS deltas.

## Method

- The baseline harness uses seven timed samples and reports the median and best time per operation. Fixture construction is outside the timed region.
- Registry fixtures use complete level-order Deque topologies with one element per Deque.
- Deque fixtures use one root and a flat list of children.
- The UI mutation fixture uses one flat sibling list. The timed operation toggles one attribute.
- The Stylo fixture drains the initial bounded stylesheet work before timing. Each timed operation toggles one class and runs a complete incremental style pass.
- The memory harness reads Linux VmRSS immediately before and after fixture construction. This is an approximate process-level delta and includes allocator/runtime overhead; it is not a per-type allocation profiler.

## Environment

Measurements recorded on 2026-11-24:

- CPU: Intel Core i7-9750H, 6 cores / 12 threads
- OS: Linux (CachyOS)
- Architecture: x86_64
- Rust: rustc 1.100.0
- Profile: optimized Cargo bench profile

## Results

### UI mutation recording

The first measurement exposed sibling snapshots being materialized twice for non-structural changes, even though attribute/text/style invalidation only consumed the parent identity. After separating the parent-only context from structural sibling snapshots:

| Sibling elements | Before | After |
|---:|---:|---:|
| 128 | 7.31 µs | 0.55 µs |
| 512 | 25.96 µs | 0.55 µs |
| 2,048 | 115.78 µs | 0.54 µs |
| 4,096 | 185.22 µs | 0.57 µs |

### Attribute mutation plus Stylo style pass

The first pass conservatively dirtied every sibling. Selector-flag-guided invalidation now expands to sibling subtrees only when the matched selector dependencies require it.

| Sibling elements | Before: recomputed / time | After: recomputed / time |
|---:|---:|---:|
| 128 | 129 / 1.81 ms | 2 / 24.5 µs |
| 512 | 513 / 5.95 ms | 2 / 26.2 µs |
| 2,048 | 2,049 / 28.1 ms | 2 / 26.2 µs |
| 4,096 | 4,097 / 63.3 ms | 2 / 19.6 µs |

The post-change numbers apply to a stylesheet with a simple class selector and no sibling dependency. Separate regression tests cover adjacent-sibling invalidation and :has(+ ...); those cases intentionally expand the dirty set.

### Structural and registry paths

Representative post-change measurements:

| Operation | Size | Median |
|---|---:|---:|
| detect_changes_for with one candidate | 8,191 Deques | 139 ns |
| prepare_render_pending | 8,191 Deques | 41 ns |
| Full RenderStack traversal | 8,191 Deques | 5.12 µs |
| Full preorder rebuild | 8,191 Deques | 76.3 µs |
| structural_range | 1,024 entries | 27 ns |

prepare_render_pending remains effectively independent of topology size because the RenderStack snapshot is shared through an Arc; rebuilding the full preorder remains linear and is reserved for topology changes.

### Approximate memory deltas

Each fixture was measured in a fresh process. Values are Linux VmRSS deltas, rounded to KiB.

| Items | DequeTree + Registry | UiModel flat sibling list |
|---:|---:|---:|
| 127 | 160 KiB | 120 KiB |
| 511 | 476 KiB | 372 KiB |
| 2,047 | 1,716 KiB | 884 KiB |
| 8,191 | 6,736 KiB | 3,468 KiB |

RSS deltas are approximate and should be used for trend comparisons on the same environment, not as exact object-size accounting.

## Interpretation

The current measurements support three conclusions:

1. Deque identity lookup and targeted registry checks do not scale with the total number of unrelated Deques.
2. Recording non-structural mutations is now approximately constant with sibling count.
3. Stylo's dirty set is now dependency-driven for non-structural mutations; structural mutations retain broad invalidation where sibling positions and topology can change.

Do not add hard timing thresholds to shared CI runners. Use these benchmarks to compare before/after changes on the same machine and toolchain.
