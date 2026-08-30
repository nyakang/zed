# NyaTerm fork notes

This branch carries [NyaTerm](https://github.com/nyakang/nyaterm)'s local changes
to GPUI on top of an unmodified upstream base.

- Fork: <https://github.com/nyakang/zed>
- Upstream: <https://github.com/zed-industries/zed>
- Base revision: `4278ff36ef` (upstream `main`)
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

## Not carried here

Nothing. The old NyaTerm `vendor/zed` snapshot was byte-identical to this branch
apart from its own `NYATERM_VENDOR.md`. In particular `livekit.yaml` and
`crates/collab/.env.toml` match upstream exactly, so there is nothing
NyaTerm-local about them to keep out of this branch.

## Validation

The series was rebased from `78712609` onto `4278ff36`, 140 upstream commits
later, without a single conflict: the only files both sides touch are
`crates/gpui/src/platform.rs`, `crates/gpui/src/window.rs`,
`crates/gpui/src/platform/test/window.rs` and `crates/gpui_windows/src/window.rs`,
and in each one the hunks are hundreds of lines apart. Upstream did not touch
`PlatformAtlas`, `AtlasKey`, `AtlasTextureKind`, `AtlasTile`, or any of the five
atlas implementations.

On Windows 11 at the new base:

```sh
cargo check -p gpui -p gpui_platform   # clean
cargo test -p gpui strided_update_preserves_pixels_outside_the_dirty_rectangle
cargo check -p gpui_windows            # clean
```

All pass, warning-free. `gpui_apple` (Metal) and `gpui_linux` cannot be compiled
on this host, so those two `update` implementations rest on
`.github/workflows/nyaterm.yml`, which checks `gpui_wgpu`/`gpui_linux` on Linux
and `gpui_apple` on macOS.

The hidden-cursor patch was additionally checked on Windows 11 with
`cargo check -p gpui -p gpui_platform -p gpui_windows`. Its focused
`gpui_windows` unit test is present, but the crate's test configuration currently
fails before running tests because `gpui_windows::WindowsWindow` exposes the
test-only `render_to_image` method while the selected `gpui::PlatformWindow`
trait does not. The normal library check is clean; Linux, macOS, and Web
exhaustive cursor matches are covered by the branch workflow.

One consumer-visible thing to know about this base rather than about the patches:
`gpui-component` declares `gpui` with `features = ["profiler"]`, so feature
unification turns the profiler on for anything that links both. Upstream's
`1861e58f98` added a per-thread foreground journal and a hang detector (about
4,000 lines) that hook `App::new`, `ForegroundExecutor::new`,
`PlatformScheduler::schedule_local`, the window frame-request closure and
`present`. The checks above use default features and never compile that path.

The owner-disabled rollback is not covered by a test: it needs one of six Win32
calls inside `WindowsWindow::new` to fail, and none of them is injectable from
outside the function. It was reviewed against `handle_destroy_msg`
(`crates/gpui_windows/src/events.rs`), which is the only other place that
re-enables an owner, so that both paths do the same two things in the same order.
