//! The popup root view: lifecycle, geometry and content.
//!
//! The host/phase/motion/region machinery is Codex Minibar's, trimmed to a
//! single page: the capsule height follows the measured content, slides in
//! from the screen edge and is clipped with `SetWindowRgn` every frame.

use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, App, AvailableSpace, ClickEvent, Context, DragMoveEvent, Entity, FontWeight,
    InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Point, Render,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement, Styled, Subscription, Window,
    canvas, div, point, prelude::*, px, svg,
};

use super::{
    assets::{icon_path, profile_glyph},
    fx::{self, Fx, Spring},
    slider::{self, DragState, SliderSpec},
    theme::{self, HslaExt, Palette},
};
use crate::app::{AppModel, MonitorRow};
use crate::monitors::BrightnessBackend;
use crate::settings::ProfileIcon;

/// Fluent motion tokens used by Windows edge panels.
const OPEN_ANIMATION: Duration = Duration::from_millis(250);
const CLOSE_ANIMATION: Duration = Duration::from_millis(167);
/// Longest frame the open slide advances through; slower frames are hitches.
const SLIDE_MAX_FRAME_STEP: Duration = Duration::from_millis(34);
/// Invisible frames rendered at launch before the popup host is parked.
const PREWARM_FRAMES: u32 = 3;
/// Following prewarm frames park the capsule one pixel from the screen edge,
/// where every open slide starts, and expose that pixel. DWM skips windows
/// with an empty region, so without this the first real open pays for the
/// backdrop's effect compilation inside DWM, a stall the slide clock cannot
/// see.
const PREWARM_EXPOSED_FRAMES: u32 = 6;
/// Small inner inset keeps content clear of the capsule stroke.
const CHROME_INSET: f32 = 1.0;
const PAGE_PADDING: f32 = 12.0;
const FOOTER_HEIGHT: f32 = 48.0;
const BUTTON: f32 = 36.0;
const GLYPH: f32 = 18.0;
/// Baseline height before the first content measurement.
const INITIAL_HEIGHT: f32 = 160.0;
const SCROLL_GLIDE: Duration = Duration::from_millis(140);
/// Profile glyph picker: two rows of tiles, a divider, then the save and
/// delete rows.
const PROFILE_MENU_COLUMNS: usize = 6;
const PROFILE_MENU_PADDING: f32 = 4.0;
const PROFILE_MENU_GAP: f32 = 4.0;
const PROFILE_MENU_INSET: f32 = 6.0;
/// Padding, border, two tile rows with their gap, the divider, the two
/// action rows with their gap and the gaps around the divider.
const PROFILE_MENU_HEIGHT: f32 =
    PROFILE_MENU_PADDING * 2.0 + 2.0 + BUTTON * 4.0 + 2.0 * 2.0 + 1.0 + PROFILE_MENU_GAP * 2.0;

/// Round the independent MaxContent measurement once to physical pixels.
fn measured_page_height(height: f32, scale: f32) -> f32 {
    let scale = scale.max(0.5);
    ((height * scale - 0.01).ceil() / scale).max(PAGE_PADDING * 2.0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Phase {
    Hidden,
    /// Launch-time invisible render (empty region) that warms the renderer,
    /// composition and glyph/icon atlases before first open.
    Prewarm,
    Opening(Instant),
    Open,
    /// Closing from the given starting slide offset (DIP).
    Closing(Instant, f32),
}

/// Native host state. All geometry is in DIP unless suffixed `_px`.
pub(crate) struct Host {
    #[cfg(windows)]
    pub(crate) hwnd: Option<windows_sys::Win32::Foundation::HWND>,
    #[cfg(windows)]
    backdrop: Option<super::backdrop::Backdrop>,
    pub(crate) monitor: Option<super::Monitor>,
    /// The work-area corner the popup stands in.
    pub(crate) placement: super::Placement,
    pub(super) phase: Phase,
    pub(super) height: Spring,
    pub(super) offset: f32,
    pub(crate) last_region: Option<(i32, i32, i32, i32, i32)>,
    /// Capsule pixels last published for outside-click tests.
    last_surface: Option<(i32, i32, i32, i32)>,
    pub(crate) scale: f32,
    /// Previous frame of the open slide; long gaps are not counted as motion.
    slide_frame: Option<Instant>,
    /// Frames rendered during `Phase::Prewarm`.
    prewarm_frames: u32,
}

impl Host {
    fn new() -> Self {
        Self {
            #[cfg(windows)]
            hwnd: None,
            #[cfg(windows)]
            backdrop: None,
            monitor: None,
            placement: super::Placement::default(),
            phase: Phase::Hidden,
            height: Spring::at(f64::from(INITIAL_HEIGHT)),
            offset: 0.0,
            last_region: None,
            last_surface: None,
            scale: 1.0,
            slide_frame: None,
            prewarm_frames: 0,
        }
    }

    pub(crate) fn visible(&self) -> bool {
        !matches!(self.phase, Phase::Hidden)
    }

    /// The prewarm frame that shows DWM the capsule's edge pixel.
    fn prewarm_exposed(&self) -> bool {
        self.phase == Phase::Prewarm
            && (PREWARM_FRAMES..PREWARM_FRAMES + PREWARM_EXPOSED_FRAMES)
                .contains(&self.prewarm_frames)
    }

    fn max_height(&self) -> f32 {
        self.monitor.map_or(4096.0, |monitor| {
            #[cfg(windows)]
            {
                (f64::from(super::host_rect(monitor, self.placement).height()) / monitor.scale())
                    as f32
            }
            #[cfg(not(windows))]
            {
                let _ = monitor;
                4096.0
            }
        })
    }

    fn margin(&self) -> f32 {
        crate::popup::EDGE_MARGIN_PX as f32 / self.scale.max(0.5)
    }
}

pub(crate) struct PopupRoot {
    model: Entity<AppModel>,
    palette: Palette,
    default_font: SharedString,
    fx: Fx,
    hover: HashSet<u64>,
    pub(crate) host: Host,
    content_height: Rc<Cell<f32>>,
    scroll_target: f32,
    drag: DragState,
    capsule_origin: Point<gpui::Pixels>,
    capsule_size: (f32, f32),
    /// Profile whose glyph picker is open.
    profile_menu: Option<usize>,
    /// Profile being dragged to a new place in the bar.
    profile_drag: Option<ProfileDragState>,
    _subscriptions: Vec<Subscription>,
}

impl PopupRoot {
    pub(crate) fn new(
        model: Entity<AppModel>,
        default_font: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let palette = build_palette(&model, &default_font, window, cx);
        let appearance = cx.observe_window_appearance(window, |this, window, cx| {
            this.refresh_palette(window, cx);
            cx.notify();
        });
        let observe = cx.observe(&model, |this, _, cx| {
            if this.host.visible() {
                cx.notify();
            }
        });
        // Alt+F4 on the focused popup dismisses it; the HWND must survive.
        window.on_window_should_close(cx, |_, _| {
            crate::popup::hide();
            false
        });
        Self {
            model,
            palette,
            default_font,
            fx: Fx::default(),
            hover: HashSet::new(),
            host: Host::new(),
            content_height: Rc::new(Cell::new(0.0)),
            scroll_target: 0.0,
            drag: Rc::new(RefCell::new(None)),
            capsule_origin: Point::default(),
            capsule_size: (0.0, 0.0),
            profile_menu: None,
            profile_drag: None,
            _subscriptions: vec![appearance, observe],
        }
    }

    fn refresh_palette(&mut self, window: &Window, cx: &App) {
        let palette = build_palette(&self.model, &self.default_font, window, cx);
        if palette != self.palette {
            #[cfg(windows)]
            if (self.palette.material != palette.material
                || self.palette.backdrop_luminosity() != palette.backdrop_luminosity())
                && let Some(backdrop) = &mut self.host.backdrop
            {
                backdrop.set_appearance(palette.material, palette.backdrop_luminosity());
            }
            self.palette = palette;
        }
    }

    pub(crate) fn appearance_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_palette(window, cx);
        cx.notify();
    }

    // ----- lifecycle --------------------------------------------------------

    #[cfg(windows)]
    pub(crate) fn attach_native_host(&mut self, hwnd: windows_sys::Win32::Foundation::HWND) {
        self.host.hwnd = Some(hwnd);
        self.host.backdrop = match super::backdrop::Backdrop::new(
            hwnd,
            self.palette.material,
            self.palette.backdrop_luminosity(),
        ) {
            Ok(backdrop) => Some(backdrop),
            Err(error) => {
                // Keep plain transparency if this system cannot host a clipped
                // backdrop. Never fall back to blur across the whole HWND.
                crate::logger::error(format!("could not create capsule backdrop: {error:#}"));
                None
            }
        };
    }

    /// Prepares geometry for a show; the caller performs the native show
    /// outside of this update so GPUI can paint the first frame.
    #[cfg(windows)]
    pub(crate) fn begin_show(
        &mut self,
        anchor: Option<(i32, i32)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<super::ShowPlan> {
        let hwnd = self.host.hwnd?;
        let already_visible = matches!(self.host.phase, Phase::Open | Phase::Opening(_));
        let was_hidden = matches!(self.host.phase, Phase::Hidden | Phase::Prewarm);
        let anchor = anchor.unwrap_or_else(|| {
            let rect = super::win32::window_rect(hwnd);
            (rect.right - 1, rect.bottom - 1)
        });
        let monitor = super::win32::monitor_for_point(anchor.0, anchor.1);
        self.host.monitor = Some(monitor);
        self.host.scale = monitor.scale() as f32;
        // An open popup stays on its side rather than jumping across.
        if !already_visible {
            self.host.placement = super::Placement::for_monitor(monitor);
        }
        // The system accent can change while the popup is hidden.
        self.refresh_palette(window, cx);
        let rect = super::host_rect(monitor, self.host.placement);
        if !already_visible {
            let closing_offset = match self.host.phase {
                Phase::Closing(started, from) => Some(closing_offset(
                    started,
                    from,
                    self.capsule_size.0,
                    self.host.margin(),
                )),
                _ => None,
            };
            self.host.height.snap(f64::from(self.target_height()));
            let width = crate::popup::POPUP_WIDTH as f32;
            self.host.phase = if super::animations_enabled() {
                // Reopening mid-close continues from the current position.
                let full = width + self.host.margin();
                let start = closing_offset.unwrap_or(full);
                let progress_offset = 1.0 - (start / full).clamp(0.0, 1.0);
                Phase::Opening(
                    Instant::now()
                        - Duration::from_secs_f32(
                            OPEN_ANIMATION.as_secs_f32() * progress_offset * 0.5,
                        ),
                )
            } else {
                Phase::Open
            };
            // The first frame after a cold start can outlast the whole
            // slide. Count the wait until that frame as a hitch so the
            // entrance still plays.
            self.host.slide_frame = Some(Instant::now());
            self.host.last_region = None;
            self.hover.clear();
            self.scroll_target = 0.0;
            self.fx.snap(fx::key("scroll"), 0.0);
        }
        crate::popup::set_lifecycle(true, false);
        cx.notify();
        Some(super::ShowPlan {
            hwnd,
            rect,
            first_show: was_hidden,
        })
    }

    /// Prepares an invisible launch-time render at full host size, so the
    /// first real open does not pay for swap-chain growth, composition setup
    /// or glyph/icon rasterization mid-slide.
    #[cfg(windows)]
    pub(crate) fn begin_prewarm(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<super::ShowPlan> {
        let hwnd = self.host.hwnd?;
        if self.host.phase != Phase::Hidden {
            return None;
        }
        // The primary monitor owns the taskbar tray in the common setup.
        let monitor = super::win32::monitor_for_point(0, 0);
        self.host.monitor = Some(monitor);
        self.host.scale = monitor.scale() as f32;
        self.host.placement = super::Placement::for_monitor(monitor);
        self.refresh_palette(window, cx);
        self.host.height.snap(f64::from(self.target_height()));
        self.host.phase = Phase::Prewarm;
        self.host.prewarm_frames = 0;
        self.host.last_region = None;
        cx.notify();
        Some(super::ShowPlan {
            hwnd,
            rect: super::host_rect(monitor, self.host.placement),
            first_show: true,
        })
    }

    /// The prewarm rendered enough frames, or a real open already took over.
    pub(crate) fn prewarm_settled(&self) -> bool {
        self.host.phase != Phase::Prewarm
            || self.host.prewarm_frames >= PREWARM_FRAMES + PREWARM_EXPOSED_FRAMES
    }

    pub(crate) fn end_prewarm(&mut self, cx: &mut Context<Self>) {
        if self.host.phase == Phase::Prewarm {
            self.finish_hide(cx);
            cx.notify();
        }
    }

    pub(crate) fn begin_hide(&mut self, cx: &mut Context<Self>) {
        match self.host.phase {
            Phase::Hidden | Phase::Prewarm | Phase::Closing(..) => {}
            _ => {
                // Dismissing the popup without answering the import offer declines it.
                if self.model.read(cx).import_offer.is_some() {
                    self.model
                        .update(cx, |model, cx| model.answer_import(false, cx));
                }
                let from = self.host.offset;
                if super::animations_enabled() {
                    self.host.phase = Phase::Closing(Instant::now(), from);
                    crate::popup::set_lifecycle(true, true);
                } else {
                    self.finish_hide(cx);
                }
                cx.notify();
            }
        }
    }

    fn finish_hide(&mut self, cx: &mut Context<Self>) {
        self.host.phase = Phase::Hidden;
        #[cfg(windows)]
        if let Some(backdrop) = &mut self.host.backdrop {
            backdrop.hide();
        }
        self.host.offset = 0.0;
        self.profile_menu = None;
        self.profile_drag = None;
        self.hover.clear();
        *self.drag.borrow_mut() = None;
        self.fx = Fx::default();
        crate::popup::set_lifecycle(false, false);
        crate::popup::publish_surface_bounds(0, 0, 0, 0);
        #[cfg(windows)]
        if let Some(hwnd) = self.host.hwnd {
            // ShowWindow re-enters GPUI; run it after this frame.
            cx.spawn(async move |this, cx| {
                // A tray click may reopen between the last closing frame
                // and this task. Never park an already-reopened popup.
                let hidden = this
                    .update(cx, |root, _| !root.host.visible())
                    .unwrap_or(false);
                if hidden {
                    super::win32::set_region(hwnd, None, 0);
                    super::win32::hide(hwnd);
                    super::win32::park_hidden(hwnd);
                }
            })
            .detach();
        }
        self.host.last_region = None;
        crate::idle::enter_soon(cx);
    }

    // ----- geometry ---------------------------------------------------------

    fn chrome_height(&self) -> f32 {
        FOOTER_HEIGHT + CHROME_INSET * 2.0
    }

    fn target_height(&self) -> f32 {
        let chrome = self.chrome_height();
        let mut content = Some(self.content_height.get())
            .filter(|height| *height > 1.0)
            .unwrap_or(INITIAL_HEIGHT - chrome);
        // The open glyph picker never hangs past the capsule's far edge.
        if self.profile_menu.is_some() {
            content = content.max(PROFILE_MENU_HEIGHT + PROFILE_MENU_INSET * 2.0);
        }
        (content + chrome).clamp(80.0, self.host.max_height())
    }

    /// Advance window motion for this frame. Returns `(height, offset)`.
    fn step_motion(&mut self, now: Instant, width: f32, cx: &mut Context<Self>) -> (f32, f32) {
        if let Phase::Opening(started) = self.host.phase {
            // Advance the slide by at most one long frame per frame: a stalled
            // first paint must not skip the entrance.
            if let Some(last) = self.host.slide_frame {
                let stall = now
                    .saturating_duration_since(last)
                    .saturating_sub(SLIDE_MAX_FRAME_STEP);
                if !stall.is_zero() {
                    self.host.phase = Phase::Opening(started + stall);
                }
            }
            self.host.slide_frame = Some(now);
        } else {
            self.host.slide_frame = None;
        }
        let target_h = self.target_height();
        let animate =
            super::animations_enabled() && self.host.visible() && self.host.phase != Phase::Prewarm;
        let opening_early = matches!(self.host.phase, Phase::Opening(started)
            if now.saturating_duration_since(started).as_secs_f32() < OPEN_ANIMATION.as_secs_f32() * 0.35);
        if animate && !opening_early {
            self.host.height.retarget(f64::from(target_h));
        } else {
            // Hidden pixels never need a glide: the first frames of the open
            // slide are still beyond the monitor edge.
            self.host.height.snap(f64::from(target_h));
        }
        let height = self.host.height.step(now) as f32;
        if !self.host.height.settled {
            self.fx.mark_animating();
        }
        let travel = width + self.host.margin();
        let offset = match self.host.phase {
            Phase::Prewarm if self.host.prewarm_exposed() => travel - 1.0,
            Phase::Hidden | Phase::Prewarm | Phase::Open => 0.0,
            Phase::Opening(started) => {
                let progress = now.saturating_duration_since(started).as_secs_f64()
                    / OPEN_ANIMATION.as_secs_f64();
                if progress >= 1.0 {
                    self.host.phase = Phase::Open;
                    #[cfg(windows)]
                    if let Some(hwnd) = self.host.hwnd {
                        cx.spawn(async move |_, _| super::win32::activate(hwnd))
                            .detach();
                    }
                    crate::popup::arm_outside_click_grace();
                    0.0
                } else {
                    self.fx.mark_animating();
                    (1.0 - crate::popup::ease_entrance(progress)) as f32 * travel
                }
            }
            Phase::Closing(started, from) => {
                let offset = closing_offset(started, from, width, self.host.margin());
                if offset >= travel - 0.5 {
                    self.finish_hide(cx);
                    travel
                } else {
                    self.fx.mark_animating();
                    offset
                }
            }
        };
        self.host.offset = offset;
        (height, offset)
    }

    /// Clip the HWND to `clip`, keep the backdrop on the capsule and
    /// publish the capsule's screen bounds.
    fn apply_region(
        &mut self,
        window: &Window,
        capsule: (f32, f32, f32, f32),
        clip: (f32, f32, f32, f32),
    ) {
        #[cfg(windows)]
        {
            let (x, y, width, height) = capsule;
            let Some(hwnd) = self.host.hwnd else {
                return;
            };
            if !self.host.visible() {
                return;
            }
            let scale = window.scale_factor();
            if let Some(backdrop) = &mut self.host.backdrop {
                backdrop.update(
                    x * scale,
                    y * scale,
                    width * scale,
                    height * scale,
                    crate::popup::corner_radius_dip() as f32 * scale,
                );
            }
            let viewport = window.viewport_size();
            let window_w = (f32::from(viewport.width) * scale).round() as i32;
            let window_h = (f32::from(viewport.height) * scale).round() as i32;
            if self.host.phase == Phase::Prewarm {
                // Render everything, show at most the capsule's edge pixel at
                // the screen edge, publish no hit bounds.
                let region = if self.host.prewarm_exposed() {
                    let y = (((y + height * 0.5) * scale).round() as i32).clamp(0, window_h - 1);
                    let x = if self.host.placement.left {
                        0
                    } else {
                        window_w - 1
                    };
                    (x, y, x + 1, y + 1, 0)
                } else {
                    (0, 0, 0, 0, 0)
                };
                if self.host.last_region != Some(region) {
                    self.host.last_region = Some(region);
                    let (left, top, right, bottom, _) = region;
                    super::win32::set_region(
                        hwnd,
                        Some(super::win32::Rect {
                            left,
                            top,
                            right,
                            bottom,
                        }),
                        0,
                    );
                }
                return;
            }
            let pixels = |x: f32, y: f32, width: f32, height: f32| {
                let left = ((x * scale).round() as i32).clamp(0, window_w);
                let right = (((x + width) * scale).round() as i32).clamp(left, window_w);
                let top = ((y * scale).round() as i32).clamp(0, window_h);
                let bottom = (((y + height) * scale).round() as i32).clamp(top, window_h);
                (left, top, right, bottom)
            };
            let surface = pixels(x, y, width, height);
            let (left, top, right, bottom) = pixels(clip.0, clip.1, clip.2, clip.3);
            let radius = (crate::popup::corner_radius_dip() as f32 * scale).round() as i32;
            let region = (left, top, right, bottom, radius);
            let region_changed = self.host.last_region != Some(region);
            if region_changed {
                self.host.last_region = Some(region);
                super::win32::set_region(
                    hwnd,
                    (right > left && bottom > top).then_some(super::win32::Rect {
                        left,
                        top,
                        right,
                        bottom,
                    }),
                    radius,
                );
            }
            // The clip can stay put while the capsule resizes inside it;
            // outside clicks still go by the visible capsule.
            if region_changed || self.host.last_surface != Some(surface) {
                self.host.last_surface = Some(surface);
                let (left, top, right, bottom) = surface;
                let origin = super::win32::window_rect(hwnd);
                crate::popup::publish_surface_bounds(
                    origin.left + left,
                    origin.top + top,
                    origin.left + right,
                    origin.top + bottom,
                );
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (window, capsule, clip);
        }
    }

    /// The HWND clip for this frame. `SetWindowRgn` makes DWM recompose the
    /// window synchronously, too slow to repeat on every frame of a resize.
    /// While the height spring runs, clip once to the union of the current
    /// and final capsule; the capsule paints its own rounded edge.
    fn region_bounds(
        &self,
        capsule: (f32, f32, f32, f32),
        viewport: (f32, f32),
        offset: f32,
    ) -> (f32, f32, f32, f32) {
        if self.host.phase != Phase::Open || self.host.height.settled {
            return capsule;
        }
        let (x, y, width, height) = capsule;
        let target_h = (self.host.height.target as f32).min(viewport.1);
        let (target_x, target_y) = self.capsule_position(viewport, (width, target_h), offset);
        let left = x.min(target_x);
        let top = y.min(target_y);
        let right = (x + width).max(target_x + width);
        let bottom = (y + height).max(target_y + target_h);
        (left, top, right - left, bottom - top)
    }

    /// Top-left of a capsule of `size` in the host window, `offset` DIPs
    /// along its slide toward the screen edge of the placement's side.
    fn capsule_position(&self, viewport: (f32, f32), size: (f32, f32), offset: f32) -> (f32, f32) {
        let margin = self.host.margin();
        let x = if self.host.placement.left {
            margin - offset
        } else {
            viewport.0 - margin - size.0 + offset
        };
        let y = if self.host.placement.top {
            0.0
        } else {
            (viewport.1 - size.1).max(0.0)
        };
        (x, y)
    }

    // ----- hover --------------------------------------------------------------

    fn hovered(&self, id: u64) -> bool {
        self.hover.contains(&id)
    }

    fn hover_listener(
        &self,
        id: u64,
        cx: &mut Context<Self>,
    ) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
        cx.listener(move |this, hovered: &bool, _, cx| {
            let changed = if *hovered {
                this.hover.insert(id)
            } else {
                this.hover.remove(&id)
            };
            if changed {
                cx.notify();
            }
        })
    }
}

fn closing_offset(started: Instant, from: f32, width: f32, margin: f32) -> f32 {
    let travel = width + margin;
    let progress = started.elapsed().as_secs_f64() / CLOSE_ANIMATION.as_secs_f64();
    let eased = crate::popup::ease_exit(progress) as f32;
    from + (travel - from) * eased
}

fn build_palette(
    model: &Entity<AppModel>,
    default_font: &SharedString,
    window: &Window,
    cx: &App,
) -> Palette {
    let settings = &model.read(cx).settings;
    let theme = settings.theme;
    Palette::new(
        theme::resolve_dark(theme, theme::system_dark(window)),
        crate::theme::accent_ramp(),
        crate::popup::background_material(),
        default_font.clone(),
        settings.backdrop_brightness,
    )
}

fn icon(name: &str, size: f32, color: gpui::Hsla) -> gpui::Svg {
    svg()
        .path(icon_path(name))
        .size(px(size))
        .flex_none()
        .text_color(color)
}

/// Drag payload: the index of the dragged profile.
struct ProfileDrag(usize);

#[derive(Clone, Copy)]
struct ProfileDragState {
    from: usize,
    /// Slot the profile would land in if dropped now.
    slot: usize,
}

/// The profile glyph following the pointer while it is dragged.
#[derive(Clone)]
struct ProfileDragPreview {
    glyph: &'static str,
    color: gpui::Hsla,
    fill: gpui::Hsla,
    stroke: gpui::Hsla,
}

impl Render for ProfileDragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size(px(BUTTON))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .bg(self.fill)
            .border_1()
            .border_color(self.stroke)
            .child(icon(self.glyph, GLYPH, self.color))
    }
}

fn eid(value: impl Into<String>) -> gpui::ElementId {
    gpui::ElementId::Name(SharedString::from(value.into()))
}

// ----- render -------------------------------------------------------------------

impl Render for PopupRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.host.visible() {
            return div().id("popup-root").size_full();
        }
        let now = Instant::now();
        self.fx.begin_frame(super::animations_enabled());
        let width = crate::popup::POPUP_WIDTH as f32;
        let (capsule_h, offset) = self.step_motion(now, width, cx);
        // Closing can finish inside step_motion. Submit an empty last frame.
        if !self.host.visible() {
            return div().id("popup-root").size_full();
        }
        let palette = self.palette.clone();
        let viewport = window.viewport_size();
        let viewport_w = f32::from(viewport.width);
        let viewport_h = f32::from(viewport.height);
        // A top/side taskbar can leave less work area than 80% of the monitor.
        let capsule_h = capsule_h.min(viewport_h);
        let (capsule_x, capsule_y) =
            self.capsule_position((viewport_w, viewport_h), (width, capsule_h), offset);
        self.capsule_origin = point(px(capsule_x), px(capsule_y));
        self.capsule_size = (width, capsule_h);
        let clip = self.region_bounds(
            (capsule_x, capsule_y, width, capsule_h),
            (viewport_w, viewport_h),
            offset,
        );
        self.apply_region(window, (capsule_x, capsule_y, width, capsule_h), clip);

        let radius = crate::popup::corner_radius_dip() as f32;
        let body_viewport_h = (capsule_h - self.chrome_height()).max(0.0);
        let page = self.render_page(width - CHROME_INSET * 2.0, body_viewport_h, window, cx);

        // A popup by a top taskbar mirrors vertically: the footer sits on the
        // capsule's edge by the taskbar.
        let chrome_on_top = self.host.placement.top;
        let (page_top, page_bottom) = if chrome_on_top {
            (self.chrome_height() - CHROME_INSET, CHROME_INSET)
        } else {
            (CHROME_INSET, self.chrome_height() - CHROME_INSET)
        };
        let page_viewport = div()
            .id("popup-page-viewport")
            .absolute()
            .top(px(page_top))
            .left(px(CHROME_INSET))
            .right(px(CHROME_INSET))
            .bottom(px(page_bottom))
            .overflow_hidden()
            .child(page);

        // Anchor the footer to the fixed host edge, not to the animated
        // capsule's top/height, so it never moves by a pixel as the height
        // spring runs.
        let footer_width = (width - CHROME_INSET * 2.0).max(0.0);
        let mut footer = div()
            .id("popup-footer")
            .absolute()
            .w(px(footer_width))
            .h(px(FOOTER_HEIGHT));
        footer = if chrome_on_top {
            footer.top(px(capsule_y + CHROME_INSET))
        } else {
            footer.bottom(px(CHROME_INSET))
        };
        footer = if self.host.placement.left {
            footer.left(px(capsule_x + CHROME_INSET))
        } else {
            footer.right(px(viewport_w - capsule_x - width + CHROME_INSET))
        };
        let footer = footer.child(self.render_footer(cx));

        #[cfg(windows)]
        let frosted = self
            .host
            .backdrop
            .as_ref()
            .is_some_and(|backdrop| backdrop.supports(palette.material));
        #[cfg(not(windows))]
        let frosted = false;
        let profile_menu = self
            .profile_menu
            .filter(|index| *index < self.model.read(cx).settings.profiles.len());
        self.profile_menu = profile_menu;
        let profile_menu = profile_menu.map(|index| self.render_profile_menu(index, cx));
        let capsule = div()
            .id("popup-capsule")
            .absolute()
            .left(px(capsule_x))
            .top(px(capsule_y))
            .w(px(width))
            .h(px(capsule_h))
            .rounded(px(radius))
            .overflow_hidden()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    // Leaving the capsule must not strand any hover state.
                    this.hover.clear();
                    cx.notify();
                }
            }))
            // A rounded composition visual below GPUI supplies blur only
            // inside the capsule. This tint leaves that backdrop visible.
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(radius))
                    .bg(palette.capsule_background(frosted)),
            )
            .child(page_viewport)
            // A hairline keeps the capsule edge crisp against any wallpaper.
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(radius))
                    .border_1()
                    .border_color(if palette.dark {
                        theme::rgba8(255, 255, 255, 0x14)
                    } else {
                        theme::rgba8(0, 0, 0, 0x12)
                    }),
            )
            .children(profile_menu.into_iter().flatten());

        if self.host.phase == Phase::Prewarm {
            self.host.prewarm_frames = self.host.prewarm_frames.saturating_add(1);
            if self.host.prewarm_frames <= PREWARM_FRAMES + PREWARM_EXPOSED_FRAMES {
                window.request_animation_frame();
            }
        }
        if self.fx.is_animating() {
            window.request_animation_frame();
        }

        div()
            .id("popup-root")
            .size_full()
            .relative()
            .font_weight(FontWeight::NORMAL)
            .font_family(palette.font_family.clone())
            .text_color(palette.text_primary)
            .text_size(px(14.0))
            .line_height(px(20.0))
            .child(capsule)
            .child(footer)
    }
}

impl PopupRoot {
    /// The scrolling content, laid out as an independent root so only its
    /// width comes from the animated viewport (breaking the content ↔ shell
    /// height loop), measured for the capsule height.
    fn render_page(
        &mut self,
        page_width: f32,
        viewport_height: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let body = self.build_body(page_width - PAGE_PADDING * 2.0, cx);
        let max_scroll = (self.content_height.get() - viewport_height).max(0.0);
        self.scroll_target = self.scroll_target.min(max_scroll);
        let scroll = self
            .fx
            .value(fx::key("scroll"), self.scroll_target, SCROLL_GLIDE)
            .clamp(0.0, max_scroll);
        let cell = Rc::clone(&self.content_height);
        let mut content = div()
            .w(px(page_width))
            .flex()
            .flex_col()
            .gap(px(self.palette.card_gap))
            .p(px(PAGE_PADDING))
            .children(body)
            .into_any_element();
        let content = canvas(
            move |bounds, window, cx| {
                let measured = content.layout_as_root(
                    gpui::size(
                        AvailableSpace::Definite(px(page_width)),
                        AvailableSpace::MaxContent,
                    ),
                    window,
                    cx,
                );
                let height =
                    measured_page_height(f32::from(measured.height), window.scale_factor());
                if cell.get() != height {
                    cell.set(height);
                    window.request_animation_frame();
                }
                content.prepaint_at(bounds.origin - point(px(0.0), px(scroll)), window, cx);
                content
            },
            |_, mut content, window, cx| content.paint(window, cx),
        )
        .size_full();
        let _ = window;
        div()
            .id("popup-page")
            .absolute()
            .inset_0()
            .overflow_hidden()
            .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, _, cx| {
                let delta = f32::from(event.delta.pixel_delta(px(20.0)).y);
                let max = (this.content_height.get() - viewport_height).max(0.0);
                let next = (this.scroll_target - delta * 2.0).clamp(0.0, max);
                if (next - this.scroll_target).abs() > 0.1 {
                    this.scroll_target = next;
                    cx.notify();
                }
            }))
            .child(content)
            .into_any_element()
    }

    fn build_body(&mut self, inner_width: f32, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let model = self.model.read(cx);
        let rows = model.rows();
        let linked = model.settings.link_levels;
        let loaded = model.loaded;
        let offer = model.import_offer.is_some();
        let mut body = Vec::new();
        if let Some(update) = crate::updater::popup_offer() {
            body.push(self.render_update_offer(update, cx));
        }
        if offer {
            body.push(self.render_import_offer(cx));
        }
        if rows.is_empty() {
            let text = if loaded {
                crate::i18n::tr("popup-no-displays")
            } else {
                crate::i18n::tr("popup-loading")
            };
            body.push(
                div()
                    .h(px(64.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(self.palette.text_secondary)
                    .child(SharedString::from(text.to_owned()))
                    .into_any_element(),
            );
            return body;
        }
        let controllable: Vec<&MonitorRow> =
            rows.iter().filter(|row| row.slider.is_some()).collect();
        let mut sections = Vec::new();
        if linked && controllable.len() > 1 {
            let average = controllable
                .iter()
                .filter_map(|row| row.slider.map(u32::from))
                .sum::<u32>()
                / controllable.len() as u32;
            let names = controllable
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let contrasts: Vec<u32> = rows
                .iter()
                .filter_map(|row| row.contrast().map(u32::from))
                .collect();
            let contrast = (!contrasts.is_empty())
                .then(|| (contrasts.iter().sum::<u32>() / contrasts.len() as u32) as u8);
            sections.push(self.render_linked_card(average as u8, contrast, names, inner_width, cx));
            // Per-display switches (HDR, power) stay reachable while levels
            // are linked.
            for row in &rows {
                sections.push(self.render_monitor_card(row, true, inner_width, cx));
            }
        } else {
            for row in &rows {
                sections.push(self.render_monitor_card(row, false, inner_width, cx));
            }
        }
        for (index, section) in sections.into_iter().enumerate() {
            if index > 0 {
                body.push(self.divider());
            }
            body.push(section);
        }
        body
    }

    /// Hairline between flat display sections.
    fn divider(&self) -> AnyElement {
        div()
            .h(px(1.0))
            .flex_none()
            .bg(self.palette.divider)
            .into_any_element()
    }

    /// A display section: card padding without the card's fill or stroke.
    fn section(&self) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .px(px(12.0))
            .py(px(2.0))
    }

    /// First-run card offering to import Twinkle Tray settings.
    fn render_update_offer(
        &mut self,
        phase: crate::updater::UpdatePhase,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::updater::UpdatePhase;
        let palette = self.palette.clone();
        let (body, buttons) = match &phase {
            UpdatePhase::Available(update) => {
                let install = self.text_button(
                    "update-install",
                    crate::i18n::tr("popup-update-install"),
                    true,
                    |_, _, _| crate::updater::install(),
                    cx,
                );
                let later = self.text_button(
                    "update-later",
                    crate::i18n::tr("popup-update-later"),
                    false,
                    |_, _, _| crate::updater::dismiss_offer(),
                    cx,
                );
                (
                    crate::i18n::format(
                        "popup-update-body",
                        &[("version", update.version.clone())],
                    ),
                    Some(
                        div()
                            .pt(px(6.0))
                            .flex()
                            .flex_row()
                            .gap(px(8.0))
                            .child(install)
                            .child(later),
                    ),
                )
            }
            UpdatePhase::Installing {
                progress: Some(fraction),
                ..
            } => (
                crate::i18n::format(
                    "about-update-downloading-percent",
                    &[("percent", format!("{:.0}", fraction * 100.0))],
                ),
                None,
            ),
            UpdatePhase::Restarting => (crate::i18n::tr("about-update-restarting"), None),
            _ => (crate::i18n::tr("about-update-downloading"), None),
        };
        self.card()
            .pb(px(12.0))
            .gap(px(4.0))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(SharedString::from(crate::i18n::tr("popup-update-title"))),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .line_height(px(16.0))
                    .text_color(palette.text_secondary)
                    .child(SharedString::from(body)),
            )
            .children(buttons)
            .into_any_element()
    }

    fn render_import_offer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let palette = self.palette.clone();
        let accept_model = self.model.clone();
        let decline_model = self.model.clone();
        let accept = self.text_button(
            "import-yes",
            crate::i18n::tr("import-prompt-yes"),
            true,
            move |_, _, cx| accept_model.update(cx, |model, cx| model.answer_import(true, cx)),
            cx,
        );
        let decline = self.text_button(
            "import-no",
            crate::i18n::tr("import-prompt-no"),
            false,
            move |_, _, cx| decline_model.update(cx, |model, cx| model.answer_import(false, cx)),
            cx,
        );
        self.card()
            .pb(px(12.0))
            .gap(px(4.0))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(SharedString::from(crate::i18n::tr("import-prompt-title"))),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .line_height(px(16.0))
                    .text_color(palette.text_secondary)
                    .child(SharedString::from(crate::i18n::tr("import-prompt-body"))),
            )
            .child(
                div()
                    .pt(px(6.0))
                    .flex()
                    .flex_row()
                    .gap(px(8.0))
                    .child(accept)
                    .child(decline),
            )
            .into_any_element()
    }

    /// Fluent accent or standard button with a hover fade.
    fn text_button(
        &mut self,
        id: &'static str,
        label: String,
        accent: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = self.palette.clone();
        let hover_id = fx::key(("button", id));
        let hovered = self.hovered(hover_id);
        let hover = self
            .fx
            .toggle(fx::key(("button-hover", id)), hovered, fx::FASTER);
        let (background, text) = if accent {
            // WinUI accent buttons fade to 90% on hover.
            (
                palette.accent.opacity(1.0 - 0.1 * hover),
                palette.text_on_accent,
            )
        } else {
            (
                palette.control_fill.mix(palette.subtle_fill, hover),
                palette.text_primary,
            )
        };
        div()
            .id(eid(format!("button-{id}")))
            .h(px(30.0))
            .px(px(14.0))
            .flex()
            .items_center()
            .rounded(px(palette.control_radius))
            .bg(background)
            .border_1()
            .border_color(palette.card_stroke)
            .text_color(text)
            .on_hover(self.hover_listener(hover_id, cx))
            .on_click(on_click)
            .child(SharedString::from(label))
            .into_any_element()
    }

    fn card(&self) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .px(px(14.0))
            .pt(px(10.0))
            .pb(px(6.0))
            .rounded(px(self.palette.card_radius))
            .bg(self.palette.card_background)
            .border_1()
            .border_color(self.palette.card_stroke)
    }

    fn card_header(
        &self,
        title: String,
        trailing: Option<String>,
        caption: Option<(String, gpui::Hsla)>,
        action: Option<AnyElement>,
    ) -> gpui::Div {
        let palette = &self.palette;
        let mut text = div().flex().flex_col().flex_1().min_w_0().child(
            div()
                .text_size(px(14.0))
                .line_height(px(20.0))
                .font_weight(FontWeight::SEMIBOLD)
                .truncate()
                .child(SharedString::from(title)),
        );
        if let Some((caption, color)) = caption {
            text = text.child(
                div()
                    .text_size(px(12.0))
                    .line_height(px(16.0))
                    .text_color(color)
                    .truncate()
                    .child(SharedString::from(caption)),
            );
        }
        let mut header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .min_h(px(32.0))
            .child(text);
        if let Some(trailing) = trailing {
            header = header.child(
                div()
                    .flex_none()
                    .min_w(px(36.0))
                    .flex()
                    .justify_end()
                    .text_size(px(14.0))
                    .text_color(palette.text_secondary)
                    .child(SharedString::from(trailing)),
            );
        }
        header.children(action)
    }

    fn slider_row(
        &mut self,
        id: String,
        leading_icon: &str,
        value: u8,
        enabled: bool,
        on_change: slider::Handler,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover_id = fx::key(("slider-hover", id.as_str()));
        let hovered = self.hovered(hover_id);
        let listener = self.hover_listener(hover_id, cx);
        let spec = SliderSpec {
            id: id.into(),
            value,
            step: self.model.read(cx).settings.slider_step(),
            enabled,
            palette: &self.palette,
            drag: Rc::clone(&self.drag),
            hovered,
            on_hover: Rc::new(move |hovered, window, cx| listener(&hovered, window, cx)),
            on_change,
        };
        let palette = self.palette.clone();
        let slider = slider::slider(spec, &mut self.fx);
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(icon(leading_icon, 18.0, palette.text_primary))
            .child(div().flex_1().min_w_0().child(slider))
            .child(
                // Fixed width keeps the track still while the digits change;
                // centered digits line up with the header's power button.
                div()
                    .flex_none()
                    .w(px(28.0))
                    .flex()
                    .justify_center()
                    .text_size(px(14.0))
                    .child(SharedString::from(value.to_string())),
            )
            .into_any_element()
    }

    fn render_linked_card(
        &mut self,
        value: u8,
        contrast: Option<u8>,
        names: String,
        _inner_width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let model = self.model.clone();
        let on_change: slider::Handler = Rc::new(move |value, _, cx| {
            model.update(cx, |model, cx| model.set_all(value, cx));
        });
        let header = self.card_header(
            crate::i18n::tr("popup-all-displays").to_owned(),
            None,
            Some((names, self.palette.text_secondary)),
            None,
        );
        let slider = self.slider_row(
            "all-displays".into(),
            "fluent-sun",
            value,
            true,
            on_change,
            cx,
        );
        let contrast = contrast.map(|contrast| {
            let model = self.model.clone();
            let on_change: slider::Handler = Rc::new(move |value, _, cx| {
                model.update(cx, |model, cx| model.set_contrast_all(value, cx));
            });
            self.slider_row(
                "all-displays-contrast".into(),
                "fluent-contrast",
                contrast,
                true,
                on_change,
                cx,
            )
        });
        self.section()
            .child(header)
            .child(slider)
            .children(contrast)
            .into_any_element()
    }

    fn render_monitor_card(
        &mut self,
        row: &MonitorRow,
        header_only: bool,
        _inner_width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = self.palette.clone();
        let unresponsive = row.monitor.unresponsive;
        let off = row.monitor.powered_off;
        // Off shows in the trailing slot: a caption line would grow the card.
        let caption = if off {
            None
        } else if unresponsive {
            Some((
                crate::i18n::tr("popup-unresponsive").to_owned(),
                palette.caution,
            ))
        } else if row.slider.is_none() && !header_only {
            Some((
                crate::i18n::tr("popup-unsupported").to_owned(),
                palette.text_tertiary,
            ))
        } else {
            None
        };
        let trailing = off.then(|| crate::i18n::tr("popup-powered-off").to_owned());
        let hdr = row.monitor.hdr_supported.then(|| {
            let on = row.monitor.sdr.is_some();
            let model = self.model.clone();
            let target = row.monitor.id.clone();
            self.header_button(
                ("hdr", &row.monitor.id),
                "fluent-hdr",
                20.0,
                on,
                move |_, _, cx| {
                    let target = target.clone();
                    model.update(cx, |model, _| model.set_hdr(&target, !on));
                },
                cx,
            )
        });
        let power = matches!(row.monitor.backend, BrightnessBackend::Ddc { .. }).then(|| {
            let model = self.model.clone();
            let target = row.monitor.id.clone();
            self.header_button(
                ("power", &row.monitor.id),
                "fluent-power",
                16.0,
                off,
                move |_, _, cx| {
                    let target = target.clone();
                    model.update(cx, |model, cx| model.set_power(&target, off, cx));
                },
                cx,
            )
        });
        let actions = (hdr.is_some() || power.is_some()).then(|| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(2.0))
                .children(hdr)
                .children(power)
                .into_any_element()
        });
        let enabled = !unresponsive && !off;
        let header = self.card_header(row.name.clone(), trailing, caption, actions);
        let mut card = self
            .section()
            .when(unresponsive, |card| card.opacity(0.7))
            .child(header);
        if !header_only && let Some(value) = row.slider {
            let model = self.model.clone();
            let id = row.monitor.id.clone();
            let on_change: slider::Handler = Rc::new(move |value, _, cx| {
                model.update(cx, |model, cx| model.set_brightness(&id, value, cx));
            });
            card = card.child(self.slider_row(
                format!("brightness-{}", row.monitor.id),
                "fluent-sun",
                value,
                enabled,
                on_change,
                cx,
            ));
        }
        if !header_only && let Some(contrast) = row.contrast() {
            let model = self.model.clone();
            let id = row.monitor.id.clone();
            let on_change: slider::Handler = Rc::new(move |value, _, cx| {
                model.update(cx, |model, cx| model.set_contrast(&id, value, cx));
            });
            card = card.child(self.slider_row(
                format!("contrast-{}", row.monitor.id),
                "fluent-contrast",
                contrast,
                enabled,
                on_change,
                cx,
            ));
        }
        // Input switching is per display, so it stays while levels are linked.
        if row.settings.show_inputs
            && row.monitor.inputs.len() > 1
            && !row.settings.visible_inputs(&row.monitor.inputs).is_empty()
        {
            card = card.child(self.input_chips(row, enabled, cx));
        }
        card.into_any_element()
    }

    /// One button per input source the display lists; the current one is
    /// filled with the accent.
    fn input_chips(
        &mut self,
        row: &MonitorRow,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = self.palette.clone();
        let mut chips = Vec::with_capacity(row.monitor.inputs.len());
        for value in row.settings.visible_inputs(&row.monitor.inputs) {
            let current = row.monitor.input == Some(value);
            let key = format!("input-{}-{value}", row.monitor.id);
            let hover_id = fx::key(("input", key.as_str()));
            let hover = self.fx.toggle(
                fx::key(("input-hover", key.as_str())),
                self.hovered(hover_id),
                fx::FASTER,
            );
            let (background, text) = if current {
                (palette.accent, palette.text_on_accent)
            } else {
                (
                    palette.control_fill.mix(palette.subtle_fill, hover),
                    palette.text_primary,
                )
            };
            let model = self.model.clone();
            let id = row.monitor.id.clone();
            chips.push(
                div()
                    .id(eid(key))
                    .h(px(26.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .rounded(px(palette.control_radius))
                    .bg(background)
                    .border_1()
                    .border_color(palette.card_stroke)
                    .text_size(px(12.0))
                    .text_color(text)
                    .on_hover(self.hover_listener(hover_id, cx))
                    .when(enabled && !current, |chip| {
                        chip.on_click(move |_, _, cx| {
                            model.update(cx, |model, cx| model.set_input(&id, value, cx));
                        })
                    })
                    .child(SharedString::from(row.settings.input_name(value)))
                    .into_any_element(),
            );
        }
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(px(4.0))
            // Line up with the slider tracks, past their leading icon.
            .pl(px(26.0))
            .pt(px(2.0))
            .pb(px(6.0))
            .when(!enabled, |row| row.opacity(0.5))
            .children(chips)
            .into_any_element()
    }

    fn render_footer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let palette = self.palette.clone();
        let model = self.model.read(cx);
        let linked = model.settings.link_levels;
        let can_link = model
            .rows()
            .iter()
            .filter(|row| row.slider.is_some())
            .count()
            > 1;
        let radius = crate::popup::corner_radius_dip() as f32;
        let link_model = self.model.clone();
        let link = can_link.then(|| {
            self.footer_button(
                "link",
                if linked {
                    "fluent-link"
                } else {
                    "fluent-link-dismiss"
                },
                linked,
                move |_, _, cx| link_model.update(cx, |model, cx| model.toggle_link_levels(cx)),
                cx,
            )
            .into_any_element()
        });
        let settings = self
            .footer_button(
                "settings",
                "fluent-settings",
                false,
                |_, _, _| crate::popup::send_command(crate::popup::PopupCommand::OpenSettings),
                cx,
            )
            .into_any_element();
        let profiles = self.render_profiles(cx);
        let corner = px((radius - 1.0).max(0.0));
        let bar = div()
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .h(px(FOOTER_HEIGHT))
            .px(px(6.0))
            .gap(px(2.0))
            .border_color(palette.card_stroke)
            .bg(palette.footer_background)
            .child(profiles)
            .children(link)
            .child(settings);
        // By a top taskbar the bar caps the capsule instead of closing it.
        let bar = if self.host.placement.top {
            bar.border_b_1().rounded_t(corner)
        } else {
            bar.border_t_1().rounded_b(corner)
        };
        bar.into_any_element()
    }

    /// Profile buttons and the quick-save "+" filling the bar's leading side.
    fn render_profiles(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let model = self.model.read(cx);
        let profiles: Vec<(ProfileIcon, bool)> = model
            .settings
            .profiles
            .iter()
            .map(|profile| (profile.icon, model.profile_active(profile)))
            .collect();
        let can_save = model.can_save_profile();
        // A drag that ended outside the bar dropped nowhere: cancel it.
        if !cx.has_active_drag() {
            self.profile_drag = None;
        }
        let drag = self
            .profile_drag
            .filter(|drag| drag.from < profiles.len() && drag.slot < profiles.len());
        // While dragging, the dragged profile already stands in its new slot.
        let mut order: Vec<usize> = (0..profiles.len()).collect();
        if let Some(drag) = drag {
            let moved = order.remove(drag.from);
            order.insert(drag.slot, moved);
        }
        let mut buttons = Vec::with_capacity(profiles.len() + 1);
        for (slot, index) in order.into_iter().enumerate() {
            let (glyph, active) = profiles[index];
            let apply_model = self.model.clone();
            let this = cx.entity();
            let dragged = drag.is_some_and(|drag| drag.from == index);
            let preview = ProfileDragPreview {
                glyph: profile_glyph(glyph),
                color: self.palette.accent,
                fill: self.palette.solid_background,
                stroke: self.palette.card_stroke,
            };
            let drag_root = cx.entity();
            let button = self
                .footer_button(
                    &format!("profile-{index}"),
                    profile_glyph(glyph),
                    active || self.profile_menu == Some(index),
                    move |_, _, cx| {
                        this.update(cx, |root, cx| {
                            root.profile_menu = None;
                            cx.notify();
                        });
                        apply_model.update(cx, |model, cx| model.apply_profile(index, cx));
                    },
                    cx,
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |root, _: &MouseDownEvent, _, cx| {
                        root.profile_menu = if root.profile_menu == Some(index) {
                            None
                        } else {
                            Some(index)
                        };
                        cx.notify();
                    }),
                )
                .on_drag(ProfileDrag(index), move |_, _, _, cx| {
                    drag_root.update(cx, |root, cx| {
                        root.profile_menu = None;
                        root.profile_drag = Some(ProfileDragState {
                            from: index,
                            slot: index,
                        });
                        cx.notify();
                    });
                    cx.new(|_| preview.clone())
                })
                .on_drag_move::<ProfileDrag>(cx.listener(
                    move |root, event: &DragMoveEvent<ProfileDrag>, _, cx| {
                        if let Some(drag) = &mut root.profile_drag
                            && drag.slot != slot
                            && event.bounds.contains(&event.event.position)
                        {
                            drag.slot = slot;
                            cx.notify();
                        }
                    },
                ))
                .when(dragged, |button| button.opacity(0.35));
            buttons.push(button.into_any_element());
        }
        if can_save {
            let save_model = self.model.clone();
            let this = cx.entity();
            buttons.push(
                self.footer_button(
                    "profile-add",
                    "fluent-add",
                    false,
                    move |_, _, cx| {
                        let saved = save_model.update(cx, |model, cx| model.save_profile(cx));
                        // Open the glyph picker so the new profile gets its icon.
                        this.update(cx, |root, cx| {
                            root.profile_menu = saved;
                            cx.notify();
                        });
                    },
                    cx,
                )
                .into_any_element(),
            );
        }
        div()
            .id("profiles")
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .overflow_x_scroll()
            .on_drop(cx.listener(|root, dropped: &ProfileDrag, _, cx| {
                if let Some(drag) = root.profile_drag.take()
                    && drag.from == dropped.0
                    && drag.from != drag.slot
                {
                    root.model
                        .update(cx, |model, cx| model.move_profile(drag.from, drag.slot, cx));
                }
                cx.notify();
            }))
            .children(buttons)
            .into_any_element()
    }

    /// Glyph picker and delete action for one profile, above the bar.
    /// Returns the dismissing scrim and the flyout.
    fn render_profile_menu(&mut self, index: usize, cx: &mut Context<Self>) -> [AnyElement; 2] {
        let palette = self.palette.clone();
        let current = self
            .model
            .read(cx)
            .settings
            .profiles
            .get(index)
            .map(|profile| profile.icon);
        let dismiss =
            |root: &mut Self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>| {
                root.profile_menu = None;
                cx.notify();
            };
        let scrim = div()
            .id("profile-menu-scrim")
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(dismiss))
            .on_mouse_down(MouseButton::Right, cx.listener(dismiss))
            .into_any_element();
        let mut tiles = Vec::with_capacity(ProfileIcon::ALL.len());
        for glyph in ProfileIcon::ALL {
            let model = self.model.clone();
            let this = cx.entity();
            let name = profile_glyph(glyph);
            tiles.push(
                self.footer_button(
                    name,
                    name,
                    current == Some(glyph),
                    move |_, _, cx| {
                        model.update(cx, |model, cx| model.set_profile_icon(index, glyph, cx));
                        this.update(cx, |root, cx| {
                            root.profile_menu = None;
                            cx.notify();
                        });
                    },
                    cx,
                )
                .into_any_element(),
            );
        }
        let save = self.profile_menu_row(
            "profile-save",
            "fluent-save",
            "popup-profile-save",
            move |model, cx| model.recapture_profile(index, cx),
            cx,
        );
        let delete = self.profile_menu_row(
            "profile-delete",
            "fluent-delete",
            "popup-profile-delete",
            move |model, cx| model.delete_profile(index, cx),
            cx,
        );
        let offset = self.chrome_height() + PROFILE_MENU_INSET - CHROME_INSET;
        let menu = div()
            .id("profile-menu")
            .absolute()
            .left(px(PROFILE_MENU_INSET))
            .h(px(PROFILE_MENU_HEIGHT))
            .p(px(PROFILE_MENU_PADDING))
            .flex()
            .flex_col()
            .gap(px(PROFILE_MENU_GAP))
            .rounded(px(palette.card_radius))
            .bg(palette.solid_background)
            .border_1()
            .border_color(palette.card_stroke)
            .occlude()
            .child(
                div()
                    .w(px(PROFILE_MENU_COLUMNS as f32 * (BUTTON + 2.0) - 2.0))
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(2.0))
                    .children(tiles),
            )
            .child(self.divider())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(save)
                    .child(delete),
            );
        let menu = if self.host.placement.top {
            menu.top(px(offset))
        } else {
            menu.bottom(px(offset))
        };
        [scrim, menu.into_any_element()]
    }

    /// Text action in the profile menu; closes the menu after `action`.
    fn profile_menu_row(
        &mut self,
        id: &'static str,
        glyph: &'static str,
        label: &str,
        action: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = self.palette.clone();
        let model = self.model.clone();
        let this = cx.entity();
        let hover_id = fx::key(("profile-menu-row", id));
        let hover = self.fx.toggle(
            fx::key(("profile-menu-row-hover", id)),
            self.hovered(hover_id),
            fx::FASTER,
        );
        let mut row = div()
            .id(id)
            .relative()
            .h(px(BUTTON))
            .px(px(9.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .rounded(px(6.0))
            .on_hover(self.hover_listener(hover_id, cx))
            .on_click(move |_, _, cx| {
                model.update(cx, |model, cx| action(model, cx));
                this.update(cx, |root, cx| {
                    root.profile_menu = None;
                    cx.notify();
                });
            });
        if hover > 0.001 {
            row = row.child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(6.0))
                    .bg(palette.subtle_fill.opacity(hover)),
            );
        }
        row.child(icon(glyph, GLYPH, palette.chrome_icon))
            .child(SharedString::from(crate::i18n::tr(label).to_owned()))
            .into_any_element()
    }

    /// Toggle in a display's card header (HDR, DDC/CI power), tinted with
    /// the accent while `active`.
    fn header_button(
        &mut self,
        (kind, id): (&str, &str),
        glyph: &'static str,
        glyph_size: f32,
        active: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = self.palette.clone();
        let hover_id = fx::key((kind, id));
        let hovered = self.hovered(hover_id);
        let hover = self
            .fx
            .toggle(fx::key((kind, "hover", id)), hovered, fx::FASTER);
        let emphasis = self
            .fx
            .toggle(fx::key((kind, "emphasis", id)), hovered || active, fx::FAST);
        let mut element = div()
            .id(eid(format!("{kind}-{id}")))
            .relative()
            .flex_none()
            .size(px(28.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(palette.control_radius))
            .on_hover(self.hover_listener(hover_id, cx))
            .on_click(on_click);
        if hover > 0.001 {
            element = element.child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(palette.control_radius))
                    .bg(palette.subtle_fill.opacity(hover)),
            );
        }
        element
            .child(icon(
                glyph,
                glyph_size,
                palette.text_secondary.mix(palette.accent, emphasis),
            ))
            .into_any_element()
    }

    fn footer_button(
        &mut self,
        id: &str,
        glyph: &'static str,
        active: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = self.palette.clone();
        let hover_id = fx::key(("footer", id));
        let hovered = self.hovered(hover_id);
        let hover = self
            .fx
            .toggle(fx::key(("footer-hover", id)), hovered, fx::FASTER);
        let emphasis = self.fx.toggle(
            fx::key(("footer-emphasis", id)),
            hovered || active,
            fx::FAST,
        );
        let mut element = div()
            .id(eid(format!("footer-{id}")))
            .relative()
            .flex_none()
            .size(px(BUTTON))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .on_hover(self.hover_listener(hover_id, cx))
            .on_click(on_click);
        if hover > 0.001 {
            element = element.child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(6.0))
                    .bg(palette.subtle_fill.opacity(hover)),
            );
        }
        element.child(icon(
            glyph,
            GLYPH,
            palette.chrome_icon.mix(palette.accent, emphasis),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_height_rounds_up_to_physical_pixels_without_layout_noise() {
        assert_eq!(measured_page_height(87.4, 1.5), 88.0);
        assert_eq!(
            measured_page_height(169.001, 1.0),
            measured_page_height(169.0, 1.0)
        );
        assert_eq!(measured_page_height(0.0, 1.0), PAGE_PADDING * 2.0);
    }
}
