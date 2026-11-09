# Rune: AgentOps patch

AgentOps's patch to the `rune` crate of [anomalyco/rune](https://github.com/anomalyco/rune), MIT
licensed (`LICENSE`, unchanged). It lives on the `agentops/rune-on-dev` branch of
[jeremielumandong/rune](https://github.com/jeremielumandong/rune), on upstream `dev` at
`565de6bb8a3449edd3dd6caa0c351bee11697964` (2026-11-09), so `git diff 565de6bb` on that branch
shows the whole patch. AgentOpsChat and Argus vendor its `crates/rune` (their `vendor/rune`,
which the root `Cargo.toml` patches `rune` to) and take their other Rune crates from the same
branch and revision. Before 2026-11-09 the patch sat on upstream
`4ca683561a100e16b1ce32f3576f4ade61bd2924`, a commit per change in AgentOpsChat's
`vendor/rune` history.

The patch brings Rune to parity with the low-power rendering of `vendor/gpui-fast` (Switchyard
PR #31; "Windows software rendering" in that PARITY-PATCH.md) for machines with no GPU: VMs on
WARP or llvmpipe/lavapipe, and Remote Desktop. The app decides when it applies
(`crates/studio-rune/src/platform/rendering.rs`) and turns it on through two switches; with
both off, which is the default, Rune behaves as upstream does, except for the on-demand frames
below, which apply everywhere but macOS.

## Packaging

- `Cargo.toml` (new) makes this a standalone workspace, so the app's workspace excludes it.
  Upstream's root manifest is kept as `Cargo.toml.orig`.
- `crates/rune/Cargo.toml`: the sibling crates (`procwatch`, `rune-motion`, `rune-staging`,
  `rune-taffy`) are git dependencies at the same rev instead of paths, and the `[[example]]` and
  `[[test]]` targets are left out with their sources. Only the crate's own unit tests come along.
- `themes/opencode/assets/fonts/IBMPlexMono-Regular.ttf` is a symlink to the crate's copy of the
  font, which `src/text/tests.rs` loads from that upstream path.

## Source changes

- `src/low_power.rs` (new): the switches `set_low_power_rendering` and
  `set_software_rendering_effects`, both off until the app sets them, and the pacing rule
  `next_frame_at`. `lib.rs` declares the module and re-exports the switches.
- Frame pacing (`window/draw.rs`, `window/win.rs`), when low power is on: a frame starts at least
  33 ms after the previous one started, and no sooner than that frame's cost (start to submit,
  capped at 100 ms) after it was submitted. A redraw asked for earlier becomes a retry at that
  time, which `new_events` already turns into a redraw. A continuing animation asks its next
  frame the same way. Resizing and hidden windows are not paced.
- Timer frames (`window/pacing.rs`): the minimum gap after a poll is 33 ms instead of
  `TIMER_FRAME` in low power.
- On-demand frames (`window/draw.rs`, `window/win.rs`, `frame/encoded.rs`), on every platform
  but macOS and whether low power is on or not, as GPUI's Windows on-demand frames patch did:
  every frame takes the path upstream keeps for a timer's frame, building the scene first and
  presenting only when it differs from what's on screen (`Presented::unchanged`), so a mouse
  move, click, key or update that changes nothing presents nothing (`FrameOutcome::Unchanged`).
  Upstream presented a full frame for each input, which over RDP's stream of mouse moves, on a
  software adapter, cost a core at rest. macOS is left alone: its display link paces input
  frames.
- No keep-alive after input: the Windows `STEADY` hold, which re-presented every refresh for a
  second after each change (for G-SYNC/FreeSync panels, whose refresh rate and brightness swing
  with lone presents), is removed. A window at rest after a click or a key now asks for no
  further frame, as GPUI's did. A variable-refresh display may flicker on hover again.
- Effects (`runtime/paint/build.rs`), when software effects are on: outer and inset shadows
  with a blur are skipped, which leaves crisp (blur 0) rings, and backdrop blur is not drawn.

### Software rendering (rune/soft-render)

Each of these is on only while low power is on, unless it says otherwise. GPU rendering is
unchanged.

- Damage-region redraw (`render/damage.rs`, `render/damage_tests.rs`, `window/draw.rs`,
  `window/win.rs`, `window.rs`). The window keeps its last frame in a texture of its own
  (`Retained`) and compares each frame's instances with the kept frame's: equal-length lists
  index by index, otherwise trimmed of their common prefix and suffix, then matched by hash with
  a longest increasing run for the order. An instance that appears, goes, changes or changes
  place damages what its quad can reach (`bounds`, which mirrors `base_vs`'s growth rules, cut
  by its clip ±1 px). An instance inside a layer damages what its outermost layer can reach:
  everything the layer and its inner layers draw, spread by 3σ + 2 px of their widest blur,
  cut by the layer's clip (a turned layer reaches its whole clip). Damage is merged into at
  most 4 rectangles (two join only when their union shades no more than both; otherwise they
  may overlap, since a rectangle redrawn twice comes out the same); over 70 % of the window,
  the frame redraws all of it. Each rectangle is
  scissored, filled with the clear colour (a small fullscreen-triangle pipeline with blend
  None, `ClearPipeline`), and every instance drawn again under the scissor
  (`Renderer::draw_damaged`, through `draw_layer_range`). The kept texture is then copied into
  the drawable, which needs `COPY_DST` on the surface (added in `surface_config` in low power).
  A new size, format, scale, clear colour, sRGB mode or glyph-atlas page reset
  (`Atlas::resets`) redraws everything; image texels written in place damage the image
  instances on their page. It applies only without a compositor, an alpha pass, a transparent
  window, native surfaces or materials, springs, effect slots or backdrop blur
  (`Renderer::damage_tracking_fits`); otherwise the frame draws as before and the kept frame is
  forgotten. `layers.rs`: a layer's clip scissor is now cut by the scissor already set, so a
  layer drawn inside a damaged rectangle stays inside it.
- Scrolling moves the kept pixels (`render/damage/scroll.rs`). When the plain comparison damages
  over 1/32 of the window (a chat beside open panels is a small share of a wide window), Rune looks for one whole-pixel vertical offset most top-level
  instances moved by (votes from instances equal but for their y words, the likeliest offsets
  checked against the sample, since repeating rows vote for their pitch too), the viewport clip
  most of them share, and pairs old and new instances moved by that offset (y words compared to
  1/64 px, and within one 1/64 step for those left over: a triangle's or segment's bounding
  height, derived from its points, is quantized too). A top-level layer (a rounded table the
  viewport's rect clip cuts through) pairs as a whole when every instance in it moved alike.
  An instance whose clip the viewport's edge cuts pairs when its clip, cut alike to the moved
  viewport, matches; the move then stays clear of its clip radius at the top and bottom. The kept frame's pixels inside the viewport, clear of its antialiased or faded
  edges and rounded corners, move by the offset (`Retained::blit`: two copies through a
  scratch texture, since a texture cannot copy onto itself), and only what the move does not
  account for is redrawn: the exposed strips, instances that appeared, went or changed, and
  anything that stayed over the moved area (an overlay, a scrollbar), where it is and where the
  move dragged it. A flat fill whose top and bottom edges are clear of the moved area is left
  alone. The move is used only when its damage is smaller than the plain one. The comparison
  reads the unculled instances (`Retained::damage` takes both): a culled quad's bands depend on
  where the viewport cuts it, so a tall card half scrolled out would change every frame.
  `damage_share` (exported) runs the comparison on two instance lists, for app tests.
- Pans move the kept pixels both ways (rune/render-cpu, `render/damage/scroll.rs`,
  `render/damage.rs`). Offsets with a horizontal part come from votes of instances at the same
  index, counted from the front and from the back (a pan keeps the painter order), equal but
  for their x and y words; the likeliest are checked against the sample (old instances keyed
  with x and y zeroed, sorted by y then x), as the vertical offsets are, and the one that
  accounts for the most sampled instances wins. Such a move (`Move::xs`) compares x words as it
  compares y words (moved, fixed to 1/64 px, a segment's or triangle's derived width too), cuts
  a clip the viewport's edge cuts on its left and right as well (`Pair::Cut`), keeps its radius
  plus 1 px clear of the viewport's left and right edges as of its top and bottom, and asks
  `flat_over` for a fill's edges across each axis that moves. Only the L the move exposes is
  redrawn (the strips around `to`), with what did not move with it. `Shift` has `dx`;
  `Retained::blit` copies from `(x0 - dx, y0 - dy)`. A move with no horizontal part compares x
  words byte for byte, as before. A pan is a full redraw no longer: the synthetic canvas below
  redraws 15 % of the window a step (the strips, and the minimap over the canvas, which the
  four-rectangle limit joins with the right strip).
- Cheaper scroll and pan tracking (rune/render-cpu). Pairing tries each new instance against the
  old one on the diagonal the last pair found (`j + (i - j)`) before any table; the exact and
  moved tables are sorted only when an instance misses, the leftover pass only when
  instances are left, and the longest increasing run is skipped when the pairs already
  increase. Instance hashes are FNV-1a in four lanes, mixed (`hash32`); every match is still
  checked word for word. The comparison's buffers live in `Retained` from frame to frame
  (`damage::Work`, `damage::Changed`, `damage::Run`, `scroll::Buffers`, and `Painted`'s scope
  stack), so a frame allocates nothing once they fit.
- A damaged redraw draws only what touches its rectangles (rune/render-cpu,
  `render/damage.rs` `keep_damaged`, `window/draw.rs`, `render.rs` `has_springs`).
  `draw_damaged` draws every instance again under each rectangle's scissor, and on a software
  adapter each instance costs its vertex work (and binning) in every one of those draws whether
  it shades a pixel or not: with ~14,000 instances and four rectangles, a frame that redrew a
  card ran the vertex shader 56,000 times. Before `prepare`, the window copies into `damaged`
  the culled instances whose `bounds` (a pixel more) touch a rectangle, every instance inside
  a layer (a layer's blur reads past what it composites) and the layer records, and prepares
  those. Not for a full redraw, a frame with an effect slot or a backdrop blur, or a renderer
  with GPU springs (which address instances by index); if the frame turns out not to fit damage
  tracking after `prepare`, it prepares all instances again and draws them whole.
- Overdraw culling (`render/cull.rs`, `render/cull_tests.rs`, `window/draw.rs`, without a
  compositor). `base_vs` rasterizes each quad whole and clips per pixel in `fs`, and the app
  stacks same-colour full-window panels, so a full frame shaded ~4.8× the window. Before
  `prepare`, a copy of the frame's instances is culled in three ways, each leaving a culled
  instance as the empty band `uv = [0, 0, -1, 0]` and a partly culled plain or shadow quad as
  what is left of it: one band, or, in a frame without native surfaces (`cull(.., split)`,
  where nothing addresses instances by index), up to 12 bands, one instance each. Greedy:
  the biggest covering rectangles go first, and one that would make too many bands is left
  drawn; one band must save a quarter of the quad, more 1024 px each, and at least 4096 px:
  1. hidden quads: walking back from the top, an instance whose touched pixels lie inside the
     rectangles later opaque fills paint (opaque `KIND_RECT`, no blur, fade or texture; its
     border-inset rect and two corner-free bands, cut to its clip less its clip radii; aligned
     rectangles joined, at most 32 kept) draws nothing, and one they cover in part draws the
     rest. Skipped in a frame with effects or backdrop blur, and nothing inside a layer counts;
  2. same-colour fills: walking forward, a flat opaque fill at the top level inside a region
     already painted its exact colour draws nothing, and an opaque one (a bordered card) over
     such a region draws only the rest (its border and corners); what draws over a region
     splits it. A fill's region is only what step 1 left of it, so a panel over the window's
     own colour still draws where the window's fill was left out under it;
  3. band to clip: a `KIND_RECT`/`KIND_SHADOW` quad its clip cuts (clip ±1 px no longer holds
     it) gets the band trick (`render/paint/bands.rs`) for the part inside the clip, so the
     shader runs only where the clip shows it.
  The window keeps a `Culler` (`window/win.rs`), whose buffers last from frame to frame
  (rune/render-cpu): `Keep::Bands` indexes one arena of rectangles instead of owning a Vec, and
  `hidden`'s and `same_colour`'s region lists grow to 48 before one linear selection keeps the
  32 biggest (`keep_biggest`). They were sorted each time an instance split them past 32, which
  nearly every glyph over a card does (Argus's profile: `same_colour`'s sort, ~11 % of a frame).
  `damage.rs` `bounds` now honours bands as well (an empty band reaches nothing), and
  `Painted::quads` counts what the quads rasterize, for `RedrawnPixels::quads`.
- Builds that change nothing are paced too (`window/draw.rs`): a frame built and found
  unchanged sets `paced_until` like a drawn one (`low_power::pace_after`), so a stream of wakes
  or events that change nothing builds at most one frame per pace (about 30 a second) instead
  of one each. On Windows under WARP that stream reached ~350 builds a second.
- Wake causes (`low_power.rs`, `window/any_host.rs`): `WakeCause` counters for what woke the
  window (app updates, live values, other wakes, timers, retries, redraws, pointer, keys, IME,
  window events), read with `take_wake_causes`, and `redrawn_pixels` (`RedrawnPixels`: pixels
  shaded, window pixels, and the pixels the frames' quads cover, i.e. overdraw). The app prints
  them in `AGENTOPSCHAT_FRAME_LOG=1` lines.
- IME (`window/platform_sync.rs`, `window/events.rs`, whether low power is on or not): the IME
  cursor area is handed to the platform only when it changes (it was set every frame), and
  `Ime::Enabled`/`Disabled` no longer ask for a frame; only preedit and commit do.
- `Gpu::window_adapter_info` (`render.rs`): the adapter `gpu/pick.rs` chooses for windows (it
  honours `WGPU_ADAPTER_NAME`), so the app decides low power from the adapter actually used.
- Bogus drawable sizes (`window.rs` `drawable_size`, `window/win.rs`, whether low power is on or
  not): a resize to an edge past the device's `max_texture_dimension_2d` (Windows reported a
  client height of 65527, a negative one wrapped, right after a `SetWindowPos` under WARP)
  keeps the old drawable instead of configuring the surface with it, which panicked in
  `Surface::configure`. A window created at such a size draws at the largest the device takes.
- Glyph work is always presented (`window/presented.rs`, whether low power is on or not): the
  frame at rest records the glyph atlas's `(rasterized, resets)` counts, and a later frame
  counts as the same only when they still match, as well as the pending uploads, so a glyph
  rasterized or placed again since is presented even if an unpresented frame already took its
  upload.

### App motion (rune/motion)

- `El::animated_changes` (`el/motion.rs`, `el/visual.rs`, `runtime/commit/flatten.rs`,
  `runtime/commit/memo_reuse.rs`): under an `El::instant_changes` ancestor, the element's subtree
  animates again: its layout, opacity and transforms spring, and what it gains or loses plays its
  transition. Its own entrance and exit follow its ancestor. The app's window lands every content
  swap at once with `instant_changes` on its root; the composer opts back in for its own changes
  (a field growing, chips joining and leaving), and only while nothing around it changed
  (`crates/studio-rune/src/chat/composer/motion.rs`). The flag is a `VisualCold` field, so the
  memo comparison covers it.
- Word reveal under reduce motion (`runtime/commit/apply.rs`): a reveal of 0 seconds (words shown
  at once, as `rune_kit::Markdown::streaming` sets) stays at once; a longer one is cut to 0.15 s.
  Upstream gave every reveal, 0 included, a 0.15 s fade under reduce motion, so the app's
  Animations Off, which turns reduce motion on, still faded a stream's words in.

### Scroll bars

- Upstream only reports the "Show scroll bars" preference (`Cx::scroll_bars`); nothing drew a
  scroll bar, so a list that scrolled looked like one that didn't. `runtime/scroll/bars.rs` (new)
  draws a 6 pt thumb over each overflowing scroll viewport's trailing edge (and its bottom edge on
  `scroll_x` / `scroll_xy`), sized to the view's share of the content, and a press on it drags
  the content (`runtime/input/pointer.rs`). The paint pass (`runtime/paint/build.rs`) draws a
  viewport's thumbs as its subtree ends, inside the layers around it and after those its content
  opened. Under an overlay mode (`WhenScrolling`, or `Automatic` with a trackpad) a thumb shows
  only while a hand moves its viewport (the wheel, keys, a touch, a drag and their momentum; not
  an app's scroll or a followed growing end), holds 0.9 s on a timer wake, then fades in 0.3 s
  (at once under reduce motion). Under `Always` it stays, so a still window draws nothing new.
- `Cx::show_scroll_bars` and `Cx::scroll_bar_color` (`runtime/cx.rs`, carried through
  `runtime/commit/*` as `reduced_motion` is) set the mode and colour for a commit; the app's
  Settings → Appearance → Scroll bars uses them (`crates/studio-rune/src/platform/scroll_bars.rs`).
  `El::scroll_bar_hidden` (`el/scroll.rs`) leaves a viewport without one, for tab strips and
  toolbars. `scrolled_rows_stay_crisp_and_keep_their_spacing` hides its bar, since it counts fills.
- Tests: `runtime/tests/scroll_bars.rs` and the unit tests in `runtime/scroll/bars.rs`
  (`cargo test -p rune --lib scroll_bars`).

### Device memory (rune/memory)

- The DX12 descriptor heap (`render.rs`, `Gpu::device_on`): the device asks for at most
  `DESCRIPTOR_VIEWS` (131,072) `max_non_sampler_bindings` instead of `Limits::default`'s
  1,000,000. wgpu-hal's DX12 backend makes one shader-visible CBV/SRV/UAV heap of exactly that
  many views when the device is made (`dx12/device.rs`, `capacity_views`; 1,048,576 on a
  resource binding tier 3 GPU), about 32 MB at the common 32 bytes a view, of which Rune's
  bind groups (a few views per atlas page and layer target) use a few hundred. The new heap is
  about 4 MB. A device that ran out would fail its next bind group with an out-of-memory error,
  so the cap leaves a wide margin. Vulkan and Metal ignore the limit. Settings → Diagnostics →
  Memory in the app shows the device's capacity.

### Remote Desktop presentation

- DX12 swapchain kind (`render.rs`, `Gpu::dx12_swapchain_kind`): in a Remote Desktop session
  (`GetSystemMetrics(SM_REMOTESESSION)`) DX12 surfaces present through the HWND's own DXGI
  swapchain (`DxgiFromHwnd`) instead of DirectComposition (`DxgiFromVisual`). On a Windows VM
  reached over RDP, drawing with WARP, the DirectComposition swapchain presented every frame
  (the app's frame log counted them) and the session showed a black window. An HWND swapchain
  is opaque only, so translucent windows and overlays find no alpha mode there
  (`translucency::mode`). `RUNE_DX12_SWAPCHAIN=hwnd` or `visual` forces either kind; the choice
  is made once per process, so a console session later reconnected over RDP keeps
  DirectComposition.

### Windows 10 WARP projection

- On DX12's software adapter (WARP) the uber layout's projection uniform (binding 3) takes a
  dynamic offset, always 0, so wgpu's DX12 backend binds it as a root CBV instead of a second
  CBV in the group's descriptor table (`render.rs`, `Renderer::root_projection`;
  `render/metallib.rs`, `uber_bind_entries_with`). Windows 10's WARP (10.0.19041, what a
  Windows 10 VM without a GPU draws with) read it wrong from the table: the window's projection
  came out non-zero, the vertex shader took `projection.zw` as the viewport, and every instance
  but the full-window background landed off screen, so the window showed only its background.
  Every `set_bind_group` of an uber group passes `Renderer::uber_offsets` (`render.rs`,
  `render/layers.rs`, `render/backdrop_blur.rs`).
- GPUs keep the descriptor table, so `uber_abi_hash` and the precompiled DXBC and metallib are
  unchanged. On WARP the precompiled DXBC still loads: the HLSL and bytecode are the same for
  both layouts, only the root signature differs.
- The window's projection is its own zeroed 16-byte buffer (`Renderer::window_projection`)
  rather than an unwritten slot past the globals, which shrink to their 32 bytes.
- On WARP a frame's uploads (instances, globals, glyph rows, layer projections) are submitted
  ahead of the frame's encoder and waited for, never recorded into it (`render/upload.rs`,
  `Renderer::uploads_apart`): Windows 10's WARP let a frame's draws read buffers before the
  copies into them had landed, so a low-power damaged redraw drew differently from a whole
  frame. The wait gives up overlapping a frame's uploads with the previous frame's drawing; on
  a software adapter both run on the CPU anyway.
- Known gap: on Windows 10's WARP, 13 layer tests (`render::layers` cache, filter and rounded
  clip tests, `tooling` desktop composites) still fail, so cached, blurred or composited layers
  may draw wrong there; a renderer's first frame drew none of a cached layer's content in the
  cache tests. They pass on Windows 11's WARP and on GPUs, the window itself draws as on Windows
  11's WARP, and the `render::damage` equivalence tests pass there since the uploads go ahead.
  Root cause not found.
- Reproduced without the VM by putting Windows 10's `d3d10warp.dll` (10.0.19041, x64, from
  Microsoft's symbol server; winbindex lists the download keys) beside the exe or test binary
  and setting `WGPU_ADAPTER_NAME="Basic Render"`; D3D12 loads a WARP beside the exe before the
  system one. `--snap` then shows the empty window, and the Rune suite runs against that WARP.

### Software rendering: layer opacity

- With `set_software_rendering_effects` on, an `El::layer_opacity` dims its subtree element by
  element, as `El::dim` does, instead of drawing it through an offscreen layer
  (`runtime/paint/build.rs`: a layer opacity no longer isolates a node by itself; a real opacity
  fade still does). Upstream's kit draws disabled controls (buttons, icon buttons, menu and list
  items, selects, switches, segments, radios, text inputs) with `layer_opacity` since #208, so
  each disabled control cost an offscreen layer every frame on WARP or lavapipe, and layers are
  what Windows 10's WARP draws worst. Where a disabled control's label overlaps its fill, the
  fill now shows faintly through the label, as before #208. GPUs keep upstream's layer.
- Test: `render::layers::tests::software_rendering_dims_a_layer_opacity_instead_of_drawing_it_offscreen`
  (no layer target, and two overlapping children show through each other). A test flips the
  switch for its own thread with `low_power::with_software_rendering_effects`.

### Text field menus

- `rune::set_field_menus(false)` turns off text fields' own menu (upstream #246: spelling
  suggestions, Cut, Copy, Paste, Select All) for every window: a right-click, the Menu key or
  Shift+F10 on a field then goes to the nearest `on_secondary_press`, as before #246
  (`field_menus.rs`; `runtime/input/secondary.rs`). On by default, as upstream. AgentOpsChat
  turns it off: its own field menu hangs from a wrapper around each kit text input, and since
  #246 the field's own menu won over it.
- Test: `field_menus::tests::a_fields_own_menu_wins_unless_the_app_turns_field_menus_off`. A test
  flips the switch for its own thread with `field_menus::with_field_menus`.

## Not ported

- Presenting only the damaged area (investigated, rune/render-cpu). wgpu 30 has no
  damage-region present. wgpu-hal's Vulkan present builds a plain `VkPresentInfoKHR` (no
  `VkPresentRegionsKHR`, and `VK_KHR_incremental_present` is never enabled); its DX12 present
  calls `IDXGISwapChain3::Present(interval, flags)`, never `Present1`. Nor can the kept frame
  be copied into the drawable in part: wgpu-core tracks every acquired surface texture from
  `TextureUses::UNINITIALIZED` (Vulkan layout `UNDEFINED`, so its old contents are gone) with an
  init tracker that clears what a frame leaves unwritten before present
  (`wgpu-core/src/present.rs`, `Queue::prepare_surface_texture_for_present`), so no drawable
  ever holds the previous frame and `Retained::copy_to` copies the whole window.
  `canvas_bench_gpu` measures that copy (see Tests). What a patch would take:
  - Linux (lavapipe on X11): lavapipe advertises `VK_KHR_incremental_present`, and Mesa's
    software X11 path puts only the presented regions' rows when it uses `xcb_put_image`
    (without MIT-SHM, as Argus's profile shows; with MIT-SHM it presents a shared pixmap and
    copies nothing). A wgpu-hal patch that enables the extension in `Adapter::open` and chains
    `VkPresentRegionsKHR` from a per-present damage list in `Swapchain::present` would take
    most of the `xcb_put_image` share; the copy into the drawable stays.
  - Windows: Rune draws through wgpu's DX12 surface (`Dx12SwapchainKind::DxgiFromVisual`: a
    DirectComposition visual over a `FLIP_DISCARD` composition swapchain), and flip model takes
    `Present1` dirty rects. wgpu-hal hands out the swapchain
    (`Surface::as_hal::<Dx12>()` → `swap_chain()`), and its `discard_texture` does only the
    bookkeeping its `present` does, so Rune could call `Present1` itself and drop the
    `SurfaceTexture` instead of `Queue::present`. Not done: it skips wgpu-core's
    `prepare_surface_texture_for_present`, it can only be checked on Windows, and it would
    save little in the app's own process: the flip model hands the buffer to DWM without a
    copy, and dirty rects let DWM and an RDP session update less, in their own processes.
- Cheaper zoom: a zoom changes every instance's size, so it redraws everything. Low power
  already paces it (a frame at most every 33 ms, no sooner than its cost after the last), and
  culling keeps it to ~1.9× the window's pixels in the synthetic canvas. Drawing quads without
  antialiasing mid-gesture would need a shader variant (the uber shader computes coverage
  inline, so a flag saves next to nothing per pixel) and shows as edges that snap when the
  gesture ends; not done.

- Window moves and resizes reusing the scene: Rune already asks no frame for `Moved` unless the
  scale changes, and a resize lays the same elements out again without running the view; a
  frame either asks for presents only what changed. The tests below check both.
- Solid-quad fast paths and GPUI's DirectComposition/present-mode changes: Rune draws everything
  through one uber shader shipped as precompiled DXBC (`uber.vs.dxbc`, `uber.fs.dxbc`), so a
  fast path means new shaders that can only be built and checked on Windows. With blurred shadows
  and blur gone, a software adapter mostly draws solid and rounded quads already.
- Translucent layers are an app-side change: the app fades with `El::dim` instead of `opacity`
  in low power, which needs no offscreen layer.

## Tests

`render/canvas_bench.rs` (rune/render-cpu) has a synthetic canvas: a toolbar, a sidebar and a
minimap around a viewport of 216 cards (a shadow, a bordered body, a header, three lines of
text and two ports each), about 14,000 instances at 1600×1000. Its
`a_pan_moves_the_kept_pixels_both_ways_and_draws_what_a_whole_frame_draws` (on the GPU) pans,
wheels, drags and zooms it and compares each damage-tracked frame with a full redraw, and
checks that a pan moves the pixels sideways and redraws under 25 % of the window. Moved pixels
may come out one level off in a few places (at most 200 pixels a frame), where lavapipe's
interpolation of a quad's local coordinates rounds a step apart at its new place. The ignored
`canvas_bench_cpu` and `canvas_bench_gpu` time cull and damage, and the whole frame on the
headless adapter (damage tracked, without the copy into a drawable, and drawn whole):
`cargo test -p rune --lib canvas_bench -- --ignored --nocapture --test-threads=1`, with
`--config 'profile.dev.package.rune.opt-level=3'` for numbers that mean something.

`cargo test -p rune --lib -- low_power window::` in this folder (with the application's
`Cargo.lock` copied here, so it reuses the same dependency versions) runs the pacing rule's tests
and the window tests, among them `a_resting_window_presents_only_input_that_changes_the_screen`
(frames presented at rest, for mouse moves, a click and keys) and
`moving_a_window_reuses_its_scene_and_a_resize_draws_once`, `an_unchanged_frame_requests_no_further_frame`,
`a_drawable_never_takes_a_size_that_wrapped_below_zero` and `a_frame_that_draws_the_same_pixels_is_the_same`
(glyph work since). `low_power::frames_that_change_nothing_build_at_the_pace` checks the build
cap. `render::damage` has the damage tests, among them
`a_damage_tracked_frame_draws_what_a_full_frame_draws` (on the GPU: hover, text and glyph changes,
rows inserted and removed and wheel scrolls, each frame compared pixel for pixel with a full
redraw), `a_scroll_moves_the_kept_pixels_and_redraws_the_strip_it_exposes` and
`culled_quads_draw_what_whole_quads_draw` (on the GPU, culled with and without splitting);
`render::cull` has the culling rules' tests. The app's
`a_wheel_step_in_a_chat_redraws_only_the_strip_it_exposes` checks that wheel steps in the
seeded long chats (tables, a Mermaid chart) move the kept pixels and redraw under 15 % of the
window. Compiling the whole lib test takes more
than 14 GB of memory; on a smaller machine, turn the other test modules off in a scratch copy.
Upstream's `reporting_frames_changes_no_frame_skip_or_redraw` depends on wall-clock timing and
fails now and then under load, unpatched too (8 of 20 runs failed here at 4ca6835).
The app's `an_idle_window_draws_nothing_on_any_screen` checks that no screen keeps drawing at
rest or after mouse moves, a click, Tab or Escape, and `mouse_moves_over_a_still_area_run_no_view`
that moves alone build nothing.
