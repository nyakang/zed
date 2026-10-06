# NyaTerm fork notes

This branch carries [NyaTerm](https://github.com/nyakang/nyaterm)'s local changes
to GPUI on top of an unmodified upstream base.

- Fork: <https://github.com/nyakang/zed>
- Upstream: <https://github.com/zed-industries/zed>
- Base revision: `f8c2cc844057540ca1eac7de4f19f50d7597dead` (upstream `main` on 2026-10-01)
- Branch: `nyaterm`
- Crates touched: `gpui`, `gpui_apple`, `gpui_wgpu`, `gpui_windows`,
  `gpui_linux`, `gpui_macos`, and `gpui_web`. Nothing else in the workspace is
  modified, and no dependency is added, so `Cargo.lock` is unchanged.

NyaTerm consumes `gpui` and the platform renderer crates from one coherent
snapshot, and needs a mutable texture it can update per dirty region for the
remote desktop surface. Without it a framebuffer update rebuilds a `RenderImage`
and clones the entire framebuffer for every frame.

## Patches

1. `feat(gpui): allocate and update mutable atlas textures` — `DynamicTexture`
   and `DynamicTextureId`, `AtlasKey::DynamicTexture`, and a new
   `PlatformAtlas::update` implemented by all five atlases (Metal, WGPU,
   DirectX, Linux headless, test). Uploads are stride-aware, so a caller can
   pass rows sliced out of a larger framebuffer, and every backend validates the
   extent, stride, payload length and destination rectangle before writing.
2. `feat(gpui): add the Window dynamic-texture API` — `create_dynamic_texture`,
   `update_dynamic_texture`, `paint_dynamic_texture` and
   `remove_dynamic_texture`.
3. `test(gpui): cover strided dirty-region uploads` — asserts a strided 2x2
   update into a 4x4 texture leaves every pixel outside the rectangle unchanged.
4. `fix(gpui_windows): release a modal dialog's disabled owner if creation fails`
   — `WindowKind::Dialog` disables its owner as soon as `GetActiveWindow`
   resolves it, and six fallible steps follow before a `WindowsWindow` exists to
   own that responsibility. Any early return left the owner disabled for the
   lifetime of the process, because `Drop for WindowsWindow` never runs and so
   the `WM_DESTROY` handler that re-enables the owner is never reached.
   `DisabledOwnerGuard` makes the handover a single step.
5. `feat(gpui_windows): warn when a Dialog has no window to own it` — an invalid
   `GetActiveWindow` left `Dialog` silently non-modal (and, without an owner,
   holding a taskbar button of its own) while still returning `Ok`.
6. `feat(gpui): add a hidden cursor style` — adds `CursorStyle::Hidden` as a
   hitbox-scoped cursor style. Windows uses a null `HCURSOR`, X11 reuses its
   persistent invisible cursor, Wayland clears the pointer surface, macOS uses
   a cached transparent `NSCursor`, and Web maps the style to CSS `none`.
7. `fix(gpui_windows): route internal drags across app windows` — while Win32
   mouse capture keeps delivering a typed GPUI drag to its source HWND, resolve
   the root GPUI window under the screen-space pointer, translate coordinates
   using that window's DPI, and deliver move/up to its existing input callback.
   Ordinary capture is unchanged because routing is gated by GPUI's active
   internal-drag state; dropping over another process or the desktop remains a
   source-window cancellation.

## Not carried here

Nothing. The old NyaTerm `vendor/zed` snapshot was byte-identical to this branch
apart from its own `NYATERM_VENDOR.md`. In particular `livekit.yaml` and
`crates/collab/.env.toml` match upstream exactly, so there is nothing
NyaTerm-local about them to keep out of this branch.

## Validation

The branch first merged upstream `63b29c2edd` after the original patch series
was based on `801c087af2`. Four conflicts required manual resolution:

- `crates/gpui/src/platform.rs` moved atlas bookkeeping into `AtlasState` and
  changed `get_or_insert_with` to take an owned key. The resolution keeps that
  model, adds a read-only tile lookup, and carries `PlatformAtlas::update` on
  the platform atlas rather than its backend.
- `crates/gpui/src/platform/test/window.rs` keeps the upstream shared headless
  atlas for ordinary test windows and retains a focused pixel-recording atlas
  for dirty-region validation.
- `crates/gpui/src/window.rs` keeps upstream visibility and text-system changes
  while adapting the dynamic-texture calls to the owned-key atlas API.
- `crates/gpui_linux/src/linux/headless/window.rs` now uses GPUI's shared
  `HeadlessAtlas`; the duplicate NyaTerm-local implementation was dropped.

Metal, WGPU, and DirectX updates were moved into their `PlatformAtlas`
implementations to match the new upstream split between atlas state and backend
allocation. The hidden-cursor and Windows dialog-owner patches merged without
content conflicts.

On Windows 11 at the new base:

```sh
cargo check -p gpui -p gpui_platform -p gpui_windows
cargo test -p gpui strided_update_preserves_pixels_outside_the_dirty_rectangle
cargo fmt -p gpui -p gpui_platform -p gpui_windows -p gpui_apple \
  -p gpui_linux -p gpui_web -p gpui_wgpu -- --check
```

All pass. Rust reports only the MSVC linker messages emitted while producing
proc-macro import libraries. `gpui_apple` (Metal) and `gpui_linux` cannot be
compiled on this host, so those two `update` implementations rest on
`.github/workflows/nyaterm.yml`, which checks `gpui_wgpu`/`gpui_linux` on Linux
and `gpui_apple` on macOS.

The hidden-cursor patch was additionally checked on Windows 11 with
`cargo check -p gpui -p gpui_platform -p gpui_windows`. Its focused
`gpui_windows` unit test is present, but the crate's test configuration currently
fails before running tests because `gpui_windows::WindowsWindow` exposes the
test-only `render_to_image` method while the selected `gpui::PlatformWindow`
trait does not. The normal library check is clean; Linux, macOS, and Web
exhaustive cursor matches are covered by the branch workflow.

The owner-disabled rollback is not covered by a test: it needs one of six Win32
calls inside `WindowsWindow::new` to fail, and none of them is injectable from
outside the function. It was reviewed against `handle_destroy_msg`
(`crates/gpui_windows/src/events.rs`), which is the only other place that
re-enables an owner, so that both paths do the same two things in the same order.

The 2026-09-22 merge to `f25434f3c5` applied without conflicts. It carries the
upstream X11 expose recovery and WGPU atlas bind-group cache changes alongside
the NyaTerm dynamic-texture implementations.

The cross-window internal-drag patch was checked on Windows 11 with:

```sh
cargo check -p gpui -p gpui_platform -p gpui_windows
cargo test -p gpui typed_drag_can_be_handed_to_another_window
cargo fmt --all -- --check
```

The focused test starts a typed drag in one GPUI window and dispatches the move
and release through a second window, asserting that the second window consumes
the payload and that the application-global drag state is cleared.

## 2026-10-01 upstream merge

Merged upstream main while preserving dynamic textures, hidden cursors, modal
owner rollback and cross-window internal drag routing. The import-list conflict
in `crates/gpui/src/window.rs` keeps the dynamic texture types and adopts
upstream `InputPreference`. Upstream atlas texture lookup now returns `Option`;
a separate compatibility patch propagates a missing-texture error on DirectX
and Metal instead of assuming a released texture exists. The README review
marker is required by this repository's `.rules` for source changes.

Windows validation with the upstream-pinned Rust 1.98.1 toolchain:

```sh
cargo check -p gpui -p gpui_platform -p gpui_windows -p gpui_wgpu
cargo test -p gpui strided_update_preserves_pixels_outside_the_dirty_rectangle
cargo test -p gpui typed_drag_can_be_handed_to_another_window
cargo fmt -p gpui -p gpui_platform -p gpui_windows -p gpui_apple \
  -p gpui_linux -p gpui_web -p gpui_wgpu -- --check
```

All passed. Metal and Linux compilation require their platform CI jobs and
were not claimed as locally verified. MSVC emits proc-macro linker messages.


## 2026-10-06 deferred file drag API

Add transport-neutral VirtualFiles descriptors with cancellable, seekable content
providers. Resolution only advertises metadata; native adapters request bounded
64 KiB ranges on workers. Safe single Unicode names and case-fold duplicates are
validated before export. Add a can_drag gesture predicate to preserve modifier
selection, and an explicit virtual-file platform capability. macOS and Wayland
retain their local Files behavior and reject VirtualFiles until their native
adapters exist. The base remains 0544bd292a52fed9718e1ba9739c4ac82c41d223.

Windows validation: cargo check -p gpui -p gpui_windows; cargo test -p gpui
--features test-support virtual --lib (4 tests); existing Files promotion/re-entry
regression passed. No new filesystem/network dependency enters GPUI. macOS and
Linux compilation are not locally verified on this Windows host.
