use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::hash::Hash;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use keycodes::{to_location, to_logical};
use openharmony_ability::window::{create_os_window, WindowCreateParams, set_cursor_grab};
use openharmony_ability::xcomponent::{Action, MouseButton as OhosMouseButton, TouchEvent};
use openharmony_ability::{AxisEventData, InputSourceType, MouseAction, MouseEventData};

use openharmony_ability::{
  ime::KeyboardStatus, Configuration, Event as MainEvent, ImeEvent, InputEvent, OpenHarmonyApp,
  OpenHarmonyWaker, Rect,
};
use openharmony_ability_plugin_app_control::{AppControlExt, ColorModeExt};
use openharmony_ability_plugin_window::WindowExt;

use crate::dpi::{PhysicalPosition, PhysicalSize, Position, Size};
use crate::error::{self};
use crate::event::{self, ElementState, Force, StartCause};
use crate::event_loop::{self, ControlFlow};
use crate::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NativeKeyCode};
use crate::monitor;
use crate::window::{self, Fullscreen, ResizeDirection, Theme, WindowSizeConstraints};

mod keycodes;

pub(crate) use crate::icon::NoIcon as PlatformIcon;

static HAS_FOCUS: AtomicBool = AtomicBool::new(true);

/// App-level theme override (issue 5, 5.2 theme backfill).
/// `set_theme(Some)` writes an explicit override; `set_theme(None)` writes FOLLOW (follow system).
/// `theme()` reads this override: on FOLLOW it falls back to `app.config().color_mode`
/// (continuously refreshed by the ConfigChanged event, reflecting system truth, no
/// manual backfill needed). Global rather than per-window, because OHOS setColorMode
/// is itself global (not window-level).
const THEME_OVERRIDE_LIGHT: u8 = 0;
const THEME_OVERRIDE_DARK: u8 = 1;
const THEME_OVERRIDE_FOLLOW: u8 = 2;
static APP_THEME_OVERRIDE: AtomicU8 = AtomicU8::new(THEME_OVERRIDE_FOLLOW);

/// Last known cursor position lives in `openharmony_ability::CURSOR_POSITION_X/Y`
/// (vp, MainPage-relative), fed by the ArkTS `MainPage.onMouse` handler via the
/// `update_cursor_position` NAPI function. The NDK XComponent mouse path never
/// fires while the cursor is over the WebView (which covers the window), so it
/// cannot be the tracking source.

/// Background tokio runtime for spawning async bridge calls (fire-and-forget).
///
/// `WindowClient` methods are `async` and return `Result<()>`. tao's window
/// operation APIs (e.g. `set_inner_size`) are synchronous and return `()` — they
/// cannot `.await`. `BridgeExecutor` wraps a `tokio::runtime::Handle` from a
/// dedicated background thread (`ohos-bridge-rt`) that drives a current-thread
/// runtime. Calling `spawn(future)` sends the future to that background thread
/// to be polled. The TSFN NonBlocking call inside `WindowClient` returns
/// immediately; the ArkTS callback runs on the main thread → no deadlock.
///
/// `tokio::runtime::Handle` is `Clone + Send + Sync`, so `BridgeExecutor` is
/// safely cloneable and can be stored in both `EventLoop` and `Window`.
#[derive(Clone)]
struct BridgeExecutor {
    handle: tokio::runtime::Handle,
}

impl BridgeExecutor {
    fn new() -> Self {
        // Panics here are acceptable: this runs exactly once during EventLoop
        // construction, before the app is functional or any recovery path
        // exists. A failure to build the tokio runtime or spawn its driver
        // thread leaves the bridge (and thus all async window operations)
        // unusable, so aborting is the only sane option.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create OHOS bridge runtime");
        let handle = runtime.handle().clone();
        std::thread::Builder::new()
            .name("ohos-bridge-rt".into())
            .spawn(move || runtime.block_on(std::future::pending::<()>()))
            .expect("Failed to spawn bridge runtime thread");
        Self { handle }
    }

    /// Spawn a fire-and-forget bridge call. The result is ignored.
    fn spawn<F>(&self, future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.handle.spawn(future);
    }
}

// Tracks currently pressed keys for repeat detection.
// When a Down event arrives for a key already in this set, it's a repeat.
thread_local! {
    static PRESSED_KEYS: RefCell<HashSet<i32>> = RefCell::new(HashSet::new());
}

struct PeekableReceiver<T> {
  recv: mpsc::Receiver<T>,
  first: Option<T>,
}

impl<T> PeekableReceiver<T> {
  pub fn from_recv(recv: mpsc::Receiver<T>) -> Self {
    Self { recv, first: None }
  }

  pub fn try_recv(&mut self) -> Result<T, mpsc::TryRecvError> {
    if let Some(first) = self.first.take() {
      return Ok(first);
    }
    self.recv.try_recv()
  }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct KeyEventExtra {}

/// Map an OHOS NDK MouseButton to tao's MouseButton.
///
/// Returns `None` for `NoneButton` (no meaningful button to report).
fn ohos_mouse_button_to_tao(button: OhosMouseButton) -> Option<event::MouseButton> {
  match button {
    OhosMouseButton::LeftButton => Some(event::MouseButton::Left),
    OhosMouseButton::RightButton => Some(event::MouseButton::Right),
    OhosMouseButton::MiddleButton => Some(event::MouseButton::Middle),
    OhosMouseButton::BackButton => Some(event::MouseButton::Other(4)),
    OhosMouseButton::ForwardButton => Some(event::MouseButton::Other(5)),
    OhosMouseButton::NoneButton => None,
  }
}

pub struct EventLoop<T: 'static> {
  pub(crate) openharmony_app: OpenHarmonyApp,
  window_target: Arc<event_loop::EventLoopWindowTarget<T>>,
  _cause: StartCause,
  user_events_sender: mpsc::Sender<T>,
  user_events_receiver: Arc<RefCell<PeekableReceiver<T>>>,
  event_loop: Arc<RefCell<Option<Box<dyn FnMut(event::Event<T>) + 'static>>>>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct PlatformSpecificEventLoopAttributes {
  pub(crate) openharmony_app: Option<OpenHarmonyApp>,
}

impl Default for PlatformSpecificEventLoopAttributes {
  fn default() -> Self {
    Self {
      openharmony_app: Default::default(),
    }
  }
}

impl<T: 'static> EventLoop<T> {
  pub(crate) fn new(attributes: &PlatformSpecificEventLoopAttributes) -> Self {
    let (user_events_sender, user_events_receiver) = mpsc::channel();

    let openharmony_app = attributes.openharmony_app.as_ref().expect(
      "An `OpenHarmonyApp` as passed to lib is required to create an `EventLoop` on \
             OpenHarmony or HarmonyNext",
    );

    let bridge_executor = BridgeExecutor::new();

    Self {
      openharmony_app: openharmony_app.clone(),
      window_target: Arc::new(event_loop::EventLoopWindowTarget {
        p: EventLoopWindowTarget {
          app: openharmony_app.clone(),
          bridge_executor,
          _control_flow: Cell::new(ControlFlow::default()),
          exit: Cell::new(false),
          _marker: PhantomData,
        },
        _marker: PhantomData,
      }),
      _cause: StartCause::Init,
      user_events_sender,
      user_events_receiver: Arc::new(RefCell::new(PeekableReceiver::from_recv(user_events_receiver))),
      event_loop: Arc::new(RefCell::new(None)),
    }
  }

  pub(crate) fn window_target(&self) -> &event_loop::EventLoopWindowTarget<T> {
    &*self.window_target
  }

  // TODO: For input event, we need some real examples to test it
  // Input events originate from the *main* window's XComponent (Float sub-windows do
  // not own an XComponent / render surface). All input dispatch therefore uses
  // window_id = 0 (main window). Phase 3 (design.md D6) only routes per-window for
  // WindowResize / ContentRectChange; input remains main-window-scoped.
  fn handle_input_event(event_loop_cell: &Arc<RefCell<Option<Box<dyn FnMut(event::Event<T>) + 'static>>>>, event: &InputEvent) {
    #[allow(unreachable_patterns)]
    match event {
      InputEvent::TouchEvent(motion_event) => {
        let window_id = window::WindowId(WindowId(0));
        let device_id = event::DeviceId(DeviceId(motion_event.device_id as _));
        let action = motion_event.event_type;

        let phase = match motion_event.event_type {
          TouchEvent::Down => Some(event::TouchPhase::Started),
          TouchEvent::Up => Some(event::TouchPhase::Ended),
          TouchEvent::Move => Some(event::TouchPhase::Moved),
          TouchEvent::Cancel => Some(event::TouchPhase::Cancelled),
          _ => None,
        };

        if let Some(phase) = phase {
          for pointer in motion_event.touch_points.iter() {
            let position = PhysicalPosition {
              x: pointer.x as _,
              y: pointer.y as _,
            };
            trace!(
              "Input event {device_id:?}, {action:?}, loc={position:?}, \
                                 pointer={pointer:?}"
            );

            let event = event::Event::WindowEvent {
              window_id,
              event: event::WindowEvent::Touch(event::Touch {
                device_id,
                phase,
                location: position,
                id: pointer.id as u64,
                force: Some(Force::Normalized(pointer.force as f64)),
              }),
            };
            if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
              h(event);
            }
          }
        }
      }
      InputEvent::MouseEvent(mouse_event) => {
        Self::handle_mouse_event(event_loop_cell, mouse_event);
      }
      InputEvent::AxisEvent(axis_event) => {
        Self::handle_axis_event(event_loop_cell, axis_event);
      }
      InputEvent::KeyEvent(key) => {
        match key.code {
          keycode => {
            let state = match key.action {
              Action::Down => event::ElementState::Pressed,
              Action::Up => event::ElementState::Released,
              _ => event::ElementState::Released,
            };

            // Detect key repeat: if a Down event arrives for a key already
            // in the pressed set, it's an auto-repeat from holding the key.
            let key_raw = keycode as i32;
            let repeat = PRESSED_KEYS.with(|keys| {
              let mut keys = keys.borrow_mut();
              match key.action {
                Action::Down => !keys.insert(key_raw), // false if already present → repeat
                Action::Up => { keys.remove(&key_raw); false }
                _ => false,
              }
            });

            let native = NativeKeyCode::Ohos(keycode.into());
            let physical_key = KeyCode::Unidentified(native);
            let logical_key = to_logical(keycode);

            let event = event::Event::WindowEvent {
              window_id: window::WindowId(WindowId(0)),
              event: event::WindowEvent::KeyboardInput {
                device_id: event::DeviceId(DeviceId(key.device_id as _)),
                event: event::KeyEvent {
                  state,
                  physical_key,
                  logical_key,
                  location: to_location(keycode),
                  repeat,
                  text: None,
                  platform_specific: KeyEventExtra {},
                },
                is_synthetic: false,
              },
            };
            if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
              h(event);
            }
          }
        }
      }
      InputEvent::ImeEvent(data) => match data {
        ImeEvent::TextInputEvent(s) => {
          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event::Event::WindowEvent {
              window_id: window::WindowId(WindowId(0)),
              event: event::WindowEvent::ReceivedImeText(s.text.clone()),
            })
          }
        }
        ImeEvent::BackspaceEvent(_) => {
          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            // Mock keyboard input event
            let _ = [ElementState::Pressed, ElementState::Released].map(|state| {
              h(event::Event::WindowEvent {
                window_id: window::WindowId(WindowId(0)),
                event: event::WindowEvent::KeyboardInput {
                  device_id: event::DeviceId(DeviceId(0)),
                  event: event::KeyEvent {
                    state,
                    logical_key: Key::Backspace,
                    physical_key: KeyCode::Backspace,
                    platform_specific: KeyEventExtra {},
                    repeat: false,
                    location: KeyLocation::Standard,
                    text: None,
                  },
                  is_synthetic: false,
                },
              });
            });
          }
        }
        ImeEvent::EnterEvent(_) => {
          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            // Mock keyboard input event
            // Mock keyboard input event
            let _ = [ElementState::Pressed, ElementState::Released].map(|state| {
              h(event::Event::WindowEvent {
                window_id: window::WindowId(WindowId(0)),
                event: event::WindowEvent::KeyboardInput {
                  device_id: event::DeviceId(DeviceId(0)),
                  event: event::KeyEvent {
                    state,
                    logical_key: Key::Enter,
                    physical_key: KeyCode::Enter,
                    platform_specific: KeyEventExtra {},
                    repeat: false,
                    location: KeyLocation::Standard,
                    text: None,
                  },
                  is_synthetic: false,
                },
              });
            });
          }
        }
        ImeEvent::ImeStatusEvent(s) => match s {
          KeyboardStatus::Hide => {
            if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
              // Mock keyboard input event that make sure egui can receive the event and trigger onblur event
              let _ = [ElementState::Pressed, ElementState::Released].map(|state| {
                h(event::Event::WindowEvent {
                  window_id: window::WindowId(WindowId(0)),
                  event: event::WindowEvent::KeyboardInput {
                    device_id: event::DeviceId(DeviceId(0)),
                    event: event::KeyEvent {
                      state,
                      logical_key: Key::Enter,
                      physical_key: KeyCode::Enter,
                      platform_specific: KeyEventExtra {},
                      repeat: false,
                      location: KeyLocation::Standard,
                      text: None,
                    },
                    is_synthetic: false,
                  },
                });
              });
            }
          }
          _ => {
            warn!("Unknown openharmony_ability ime status event {s:?}")
          }
        },
      },
      _ => {
        warn!("Unknown openharmony_ability input event {event:?}")
      }
    }
  }

  /// Handle mouse events from the OHOS NDK, converting them to tao WindowEvents.
  fn handle_mouse_event(event_loop_cell: &Arc<RefCell<Option<Box<dyn FnMut(event::Event<T>) + 'static>>>>, mouse_event: &MouseEventData) {
    let window_id = window::WindowId(WindowId(0));
    // Use device_id 0 for mouse, consistent across events.
    let device_id = event::DeviceId(DeviceId(0));

    match mouse_event.action {
      MouseAction::Move => {
        // Cursor tracking is NOT done here: the NDK mouse callback never fires
        // while the cursor is over the WebView. See the CURSOR_POSITION note
        // near the top of this file.
        let position = PhysicalPosition {
          x: mouse_event.x as f64,
          y: mouse_event.y as f64,
        };
        if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
          h(event::Event::WindowEvent {
            window_id,
            event: event::WindowEvent::CursorMoved {
              device_id,
              position,
              modifiers: ModifiersState::empty(),
            },
          });
        }
      }
      MouseAction::Press => {
        if let Some(button) = ohos_mouse_button_to_tao(mouse_event.button) {
          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event::Event::WindowEvent {
              window_id,
              event: event::WindowEvent::MouseInput {
                device_id,
                state: ElementState::Pressed,
                button,
                modifiers: ModifiersState::empty(),
              },
            });
          }
        }
      }
      MouseAction::Release => {
        if let Some(button) = ohos_mouse_button_to_tao(mouse_event.button) {
          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event::Event::WindowEvent {
              window_id,
              event: event::WindowEvent::MouseInput {
                device_id,
                state: ElementState::Released,
                button,
                modifiers: ModifiersState::empty(),
              },
            });
          }
        }
      }
      MouseAction::HoverEnter => {
        if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
          h(event::Event::WindowEvent {
            window_id,
            event: event::WindowEvent::CursorEntered { device_id },
          });
        }
      }
      MouseAction::HoverLeave => {
        if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
          h(event::Event::WindowEvent {
            window_id,
            event: event::WindowEvent::CursorLeft { device_id },
          });
        }
      }
      MouseAction::None => {
        // Ignore None events
      }
    }
  }

  /// Handle axis (scroll wheel) events from the OHOS ArkUI runtime.
  fn handle_axis_event(event_loop_cell: &Arc<RefCell<Option<Box<dyn FnMut(event::Event<T>) + 'static>>>>, axis_event: &AxisEventData) {
    let window_id = window::WindowId(WindowId(0));
    let device_id = event::DeviceId(DeviceId(0));
    let is_touchpad = axis_event.source_type == InputSourceType::Touchpad;

    if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
      // Emit scroll wheel event.
      // Use PixelDelta for touchpad (pixel-based), LineDelta for mouse wheel (line-based).
      if axis_event.delta_x != 0.0 || axis_event.delta_y != 0.0 {
        let delta = if is_touchpad {
          event::MouseScrollDelta::PixelDelta(PhysicalPosition {
            x: axis_event.delta_x as f64,
            y: axis_event.delta_y as f64,
          })
        } else {
          event::MouseScrollDelta::LineDelta(axis_event.delta_x, axis_event.delta_y)
        };

        h(event::Event::WindowEvent {
          window_id,
          event: event::WindowEvent::MouseWheel {
            device_id,
            delta,
            phase: event::TouchPhase::Moved,
            modifiers: ModifiersState::empty(),
          },
        });
      }

      // Emit pinch scale as Ctrl+MouseWheel, which WebView interprets as zoom.
      // pinch_scale: 1.0 = no change, >1.0 = zoom in, <1.0 = zoom out, 0.0 = no pinch.
      if axis_event.pinch_scale != 0.0 && axis_event.pinch_scale != 1.0 {
        let zoom_delta = if axis_event.pinch_scale > 1.0 {
          // Zooming in: positive delta
          1.0
        } else {
          // Zooming out: negative delta
          -1.0
        };

        h(event::Event::WindowEvent {
          window_id,
          event: event::WindowEvent::MouseWheel {
            device_id,
            delta: event::MouseScrollDelta::LineDelta(0.0, zoom_delta),
            phase: event::TouchPhase::Moved,
            modifiers: ModifiersState::CONTROL,
          },
        });
      }
    }
  }

  pub fn run<F>(self, event_handler: F) -> ()
  where
    F: FnMut(event::Event<T>, &event_loop::EventLoopWindowTarget<T>, &mut ControlFlow),
  {
    let event_looper = Box::leak(Box::new(self));
    event_looper.run_return(event_handler);
  }

  pub fn run_return<F>(&mut self, mut event_handle: F) -> i32
  where
    F: FnMut(event::Event<T>, &event_loop::EventLoopWindowTarget<T>, &mut ControlFlow),
  {
    let mut control_flow = ControlFlow::default();
    let target = self.window_target.clone();

    {
      // SAFETY: `run_return` is exposed via the `EventLoopExtRunReturn` trait which
      // permits non-`'static` callbacks, so the user `event_handle` (and therefore the
      // closure) may not be `'static`. The `HAS_EVENT`/single-dispatch invariant plus
      // the fact that `run_return` does not return until the app exits guarantee the
      // stored closure is never invoked after its captures are invalidated: `target`
      // is an owned `Arc` (genuinely `'static`), `control_flow` is owned, and
      // `event_handle` is dropped together with the `event_loop` slot when the
      // `OpenHarmonyApp` shuts down. The transmute erases only the callback's
      // lifetime. (Removing the transmute entirely would require tightening the
      // trait bound to `F: 'static`, which the shared `EventLoopExtRunReturn` trait
      // does not permit — see ohos-decoupling-plan-v3 P1-3.)
      let handle = unsafe {
        std::mem::transmute::<Box<dyn FnMut(event::Event<T>)>, Box<dyn FnMut(event::Event<T>)>>(
          Box::new(move |e| {
            event_handle(e, &*target, &mut control_flow);
            // We need to dispatch it after every event callbacks.
            event_handle(event::Event::MainEventsCleared, &*target, &mut control_flow);
          }),
        )
      };
      self.event_loop.replace(Some(handle));
    }

    // Snapshot the shared cells as `'static` clones so the dispatch closure passed
    // to `run_loop` (which requires `F: FnMut(MainEvent) + 'static`) captures no
    // borrows of `self`.
    let event_loop_cell = self.event_loop.clone();
    let user_events_rx = self.user_events_receiver.clone();
    let window_target = self.window_target.clone();
    let app = self.openharmony_app.clone();

    app.clone().run_loop(move |event| {
      match event {
        MainEvent::SurfaceCreate { .. } => {
          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event::Event::NewEvents(StartCause::Init));
            h(event::Event::Resumed);
          }
        }
        MainEvent::SurfaceDestroy { .. } => {
          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event::Event::Suspended);
          }
        }
        MainEvent::WindowResize { window_id, size } => {
          // Phase 3 (design.md D6): route by the originating window's id instead of
          // the ZST constant. window_id comes from the ArkTS-wrapped options
          // (lifecycle.rs window_resize closure / xcomponent.rs on_surface_changed).
          let size = PhysicalSize::new(size.width as _, size.height as _);
          let event = event::Event::WindowEvent {
            window_id: window::WindowId(WindowId(window_id)),
            event: event::WindowEvent::Resized(size),
          };

          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event);
          }
        }
        MainEvent::WindowRedraw { .. } => {
          // RedrawRequested is driven by the XComponent frame callback, which is
          // the *main* window's render surface only (Float sub-windows do not own
          // an XComponent). Keep window_id = 0 (main window).
          let event = event::Event::RedrawRequested(window::WindowId(WindowId(0)));

          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event);
          }
        }
        MainEvent::ContentRectChange(content_rect) => {
          // Propagate as Resized so tauri's resize handler fires and calls
          // webview.set_bounds() with the new window dimensions.
          // Phase 3 (design.md D6): route by content_rect.window_id (populated by the
          // window_rect_change lifecycle closure from the ArkTS-wrapped windowId).
          let size = PhysicalSize::new(content_rect.rect.width as _, content_rect.rect.height as _);
          let event = event::Event::WindowEvent {
            window_id: window::WindowId(WindowId(content_rect.window_id)),
            event: event::WindowEvent::Resized(size),
          };

          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event);
          }
        }
        MainEvent::GainedFocus => {
          // Focus is an app-level UIAbility stage event (StageEventType::ACTIVE),
          // not per-Float-sub-window. Keep window_id = 0 (main window).
          HAS_FOCUS.store(true, Ordering::Relaxed);

          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event::Event::WindowEvent {
              window_id: window::WindowId(WindowId(0)),
              event: event::WindowEvent::Focused(true),
            });
          }
        }
        MainEvent::LostFocus => {
          // Focus is an app-level UIAbility stage event (StageEventType::INACTIVE).
          // Keep window_id = 0 (main window).
          HAS_FOCUS.store(false, Ordering::Relaxed);

          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event::Event::WindowEvent {
              window_id: window::WindowId(WindowId(0)),
              event: event::WindowEvent::Focused(false),
            });
          }
        }
        MainEvent::ConfigChanged { .. } => {
          // Configuration changes are app-level (EnvironmentCallback), not tied to a
          // specific window. Keep window_id = 0 (main window).
          let size = app.content_rect();
          let scale = app.scale();
          let mut size = PhysicalSize::new(size.width as _, size.height as _);
          let event = event::Event::WindowEvent {
            window_id: window::WindowId(WindowId(0)),
            event: event::WindowEvent::ScaleFactorChanged {
              new_inner_size: &mut size,
              scale_factor: scale as _,
            },
          };

          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event);
          }
        }
        MainEvent::Start => {
          // WindowStageEventType::SHOWN (window visible to user). Forwarded as
          // Event::Resumed — tao's closest lifecycle signal to OHOS "window-shown".
          // Double Resumed (alongside SurfaceCreate/Resume) is acceptable; downstream
          // tauri RunEvent::Resumed handlers must be idempotent.
          // See openspec ohos-event-lifecycle-forward.
          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event::Event::Resumed);
          }
        }
        MainEvent::Resume { .. } => {
          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event::Event::Resumed);
          }
        }
        MainEvent::SaveState { .. } => {
          // onAbilitySaveState has no tao Event/StartCause equivalent (no Autosave
          // variant). Degraded: dropped with debug log. Apps must persist state via
          // tauri RunEvent::Exit/ExitRequested or custom logic.
          // See openspec ohos-event-lifecycle-forward.
          debug!("SaveState has no tao Event equivalent; dropped (see ohos-event-lifecycle-forward)");
        }
        MainEvent::Pause => {
          debug!("App Paused - stopped running");
          // TODO: This is incorrect - will be solved in https://github.com/rust-windowing/winit/pull/3897
          // self.running = false;
        }
        MainEvent::WindowDestroy => {
          // This fires from the UIAbility `onWindowStageDestroy` lifecycle callback,
          // which corresponds to the *main* UIAbility window stage being torn down —
          // not Float sub-windows (those are destroyed via the separate ArkTS
          // destroyWindow() path drained by tauri-runtime-wry's
          // drain_pending_window_closes()). UIAbility is a singleton (enforced by the
          // UIABILITY_CREATED guard in Window::new), so at most one main window stage
          // exists; this path dispatches CloseRequested + Destroyed for it.
          // Keep window_id = 0 (main window).
          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            let e = event::Event::WindowEvent {
              window_id: window::WindowId(WindowId(0)),
              event: event::WindowEvent::CloseRequested,
            };
            h(e);
            // Also dispatch Destroyed so tauri-runtime-wry can clean up the window.
            let destroyed = event::Event::WindowEvent {
              window_id: window::WindowId(WindowId(0)),
              event: event::WindowEvent::Destroyed,
            };
            h(destroyed);
          }
        }
        MainEvent::Destroy => {
          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event::Event::LoopDestroyed);
          }
        }
        MainEvent::Input(input_event) => {
          Self::handle_input_event(&event_loop_cell, &input_event);
        }
        // OHOS: intentionally diverges from Android/iOS — always emit Event::Opened
        // even when urls is empty.
        //
        // On Android/iOS, Event::Opened is a pure "open URL" signal and is skipped
        // when urls is empty. On OHOS, `onNewWant` serves as the "re-launch" signal
        // (the OS prevents creating a second instance), so we emit Event::Opened on
        // every re-launch to allow the single-instance plugin to trigger its callback.
        // The want.parameters from the global Mutex carries system-injected fields
        // even when no URI is provided.
        //
        // Impact on other consumers:
        // - deep-link plugin: gated with #[cfg(any(macos, ios))], not affected on OHOS
        // - other consumers: typically just log the urls, no functional side effects
        MainEvent::NewWant { uri } => {
          let urls = if uri.is_empty() {
            vec![]
          } else {
            match url::Url::parse(&uri) {
              Ok(url) => vec![url],
              Err(e) => {
                log::error!("failed to parse NewWant URI '{uri}': {e}");
                vec![]
              }
            }
          };
          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event::Event::Opened { urls });
          }
        }
        MainEvent::UserEvent { .. } => {
          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            // Drain ALL pending user events on each wake, not just one.
            //
            // Async plugin commands (window/webview/event — all `async fn`)
            // resolve on tokio worker threads and send their response
            // `EvaluateScript` ("runCallback(...)") via `proxy.send_event` →
            // waker TSFN. The TSFN NonBlocking wake can be coalesced: N queued
            // events may produce only ONE `MainEvent::UserEvent`. A single
            // `try_recv` would fetch just one and leave the rest stranded until
            // the next wake (which may never come promptly), so `runCallback`
            // never runs → the JS Promise never settles → 5000ms test timeout.
            // Custom (sync) commands don't hit this: they resolve on the main
            // thread and go through `send_user_message`'s synchronous
            // main-thread branch (direct `handle_user_message`), bypassing the
            // waker/drain path entirely.
            while let Ok(event) = user_events_rx.borrow_mut().try_recv() {
              let event = event::Event::UserEvent(event);
              h(event);
            }
          }
        }
        unknown => {
          trace!("Unknown MainEvent {unknown:?} (ignored)");
        }
      };

      if window_target.p.exit.get() {
        if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
          h(event::Event::LoopDestroyed);
          // Migrate from OpenHarmonyApp::exit(0) (removed) to
          // AppControlExt::terminate(env, 0) (MainThreadSync bridge call).
          // run_loop callbacks execute on the N-API main thread, so
          // get_main_thread_env() returns Some(env).
          let env_cell = openharmony_ability::get_main_thread_env();
          let env_ref = env_cell.borrow();
          if let Some(env) = env_ref.as_ref() {
            if let Err(e) = app.terminate(env, 0) {
              log::warn!("[tao-ohos] terminate failed: {:?}", e);
            }
          } else {
            log::warn!("[tao-ohos] terminate failed: main thread Env not available");
          }
        }
      }
    });
    0
  }

  pub fn create_proxy(&self) -> EventLoopProxy<T> {
    EventLoopProxy {
      user_events_sender: self.user_events_sender.clone(),
      waker: self.openharmony_app.create_waker(),
    }
  }
}

pub struct EventLoopProxy<T: 'static> {
  user_events_sender: mpsc::Sender<T>,
  waker: OpenHarmonyWaker,
}

impl<T: 'static> EventLoopProxy<T> {
  pub fn send_event(&self, event: T) -> Result<(), event_loop::EventLoopClosed<T>> {
    self
      .user_events_sender
      .send(event)
      .map_err(|err| event_loop::EventLoopClosed(err.0))?;
    self.waker.wake();
    Ok(())
  }
}

impl<T: 'static> Clone for EventLoopProxy<T> {
  fn clone(&self) -> Self {
    EventLoopProxy {
      user_events_sender: self.user_events_sender.clone(),
      waker: self.waker.clone(),
    }
  }
}

#[derive(Clone)]
pub struct EventLoopWindowTarget<T: 'static> {
  pub(crate) app: OpenHarmonyApp,
  bridge_executor: BridgeExecutor,
  _control_flow: Cell<ControlFlow>,
  exit: Cell<bool>,
  _marker: std::marker::PhantomData<T>,
}

impl<T: 'static> EventLoopWindowTarget<T> {
  pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
    let mut v = VecDeque::with_capacity(1);
    v.push_back(MonitorHandle::new(self.app.clone()));
    v
  }

  pub fn primary_monitor(&self) -> Option<monitor::MonitorHandle> {
    Some(monitor::MonitorHandle {
      inner: MonitorHandle::new(self.app.clone()),
    })
  }

  #[inline]
  pub fn monitor_from_point(&self, x: f64, y: f64) -> Option<MonitorHandle> {
    // OHOS is single-display; return primary when the point is within the
    // default display bounds (DisplayManager physical pixels). See ohos-monitor-real-values.
    let w = self.app.display_width() as f64;
    let h = self.app.display_height() as f64;
    if w > 0.0 && h > 0.0 && x >= 0.0 && y >= 0.0 && x < w && y < h {
      Some(MonitorHandle::new(self.app.clone()))
    } else {
      None
    }
  }

  #[cfg(feature = "rwh_05")]
  #[inline]
  pub fn raw_display_handle_rwh_05(&self) -> rwh_05::RawDisplayHandle {
    unreachable!("rwh_05 is not supported on OpenHarmony");
  }

  #[cfg(feature = "rwh_06")]
  #[inline]
  pub fn raw_display_handle_rwh_06(&self) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
    Ok(rwh_06::RawDisplayHandle::Ohos(
      rwh_06::OhosDisplayHandle::new(),
    ))
  }

  pub fn cursor_position(&self) -> Result<PhysicalPosition<f64>, error::ExternalError> {
    // Fed by the ArkTS onMouse handler (vp, MainPage-relative) — see the
    // CURSOR_POSITION note near the top of this file. Convert vp → physical px.
    let scale = self.app.scale() as f64;
    let x = f64::from_bits(openharmony_ability::CURSOR_POSITION_X.load(Ordering::Relaxed)) * scale;
    let y = f64::from_bits(openharmony_ability::CURSOR_POSITION_Y.load(Ordering::Relaxed)) * scale;
    Ok(PhysicalPosition::new(x, y))
  }

  pub fn set_theme(&self, theme: Option<Theme>) {
    use openharmony_ability::ColorMode;
    // Mirror Window::set_theme: write the global override so theme() immediately reflects app intent.
    APP_THEME_OVERRIDE.store(
      match theme {
        Some(Theme::Dark) => THEME_OVERRIDE_DARK,
        Some(Theme::Light) => THEME_OVERRIDE_LIGHT,
        None => THEME_OVERRIDE_FOLLOW,
      },
      Ordering::Relaxed,
    );
    let color_mode = match theme {
      Some(Theme::Dark) => ColorMode::Dark,
      Some(Theme::Light) => ColorMode::Light,
      None => ColorMode::NoSet,
    };
    // Migrate from OpenHarmonyApp::set_color_mode (removed) to
    // ColorModeExt::set_color_mode (MainThreadSync bridge call).
    // Bridge contract: Dark=0, Light=1, NoSet=2.
    let mode_i32 = match color_mode {
      ColorMode::Dark => 0,
      ColorMode::Light => 1,
      ColorMode::NoSet => 2,
    };
    let env_cell = openharmony_ability::get_main_thread_env();
    let env_ref = env_cell.borrow();
    if let Some(env) = env_ref.as_ref() {
      if let Err(e) = self.app.set_color_mode(env, mode_i32) {
        log::warn!(
          "EventLoopWindowTarget::set_theme: failed to call set_color_mode: {:?}",
          e
        );
      }
    } else {
      log::warn!(
        "EventLoopWindowTarget::set_theme: main thread Env not available"
      );
    }
  }
}

// Phase 3 (design.md D6): WindowId was a ZST — every OHOS window hashed to the
// same key (0), so tauri-runtime-wry's window_id_map.get(&ZST) always returned the
// last-inserted window (typically the main window). Carrying the OHOS windowId
// (0 = main, >0 = Float sub-window) as the inner value makes per-window event
// routing work: distinct windows hash to distinct keys. This type lives entirely
// inside `#[cfg(target_env = "ohos")]` (platform_impl/mod.rs:29), so other
// platforms are unaffected (rule 2).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct WindowId(i64);

impl WindowId {
  pub const fn dummy() -> Self {
    WindowId(0)
  }
}

impl From<WindowId> for u64 {
  fn from(id: WindowId) -> Self {
    id.0 as u64
  }
}

impl From<u64> for WindowId {
  fn from(id: u64) -> Self {
    WindowId(id as i64)
  }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeviceId(i32);

impl DeviceId {
  pub const fn dummy() -> Self {
    DeviceId(0)
  }
}

/// OHOS window kind: determines whether this window reuses the existing
/// UIAbility container (UIAbility) or creates a new OS-level floating window (Float).
///
/// Default is UIAbility. Only one UIAbility window can exist (singleton enforced).
/// Use Float for sub-windows — requires explicit `.ohos_window_kind(Float)` on the builder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OHOSWindowKind {
  UIAbility,
  Float,
}

static UIABILITY_CREATED: AtomicBool = AtomicBool::new(false);

/// Decoration button bitfield constants (aligned with openharmony-ability ArkTS).
const FLAG_CLOSABLE: u8 = 1;
const FLAG_MAXIMIZABLE: u8 = 2;
const FLAG_MINIMIZABLE: u8 = 4;
const FLAG_RESIZABLE: u8 = 8;
const FLAG_ALL_DECORATIONS: u8 = FLAG_CLOSABLE | FLAG_MAXIMIZABLE | FLAG_MINIMIZABLE | FLAG_RESIZABLE;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PlatformSpecificWindowBuilderAttributes {
  pub label: Option<String>,
  pub window_kind: Option<OHOSWindowKind>,
}

/// Message to the per-window decor watcher task (see `Window::decor_watch`).
/// Dispatches, decor-change events and rechecks flow through the SAME
/// unbounded channel, so the watcher observes them in causal order.
enum DecorWatchMsg {
  /// A new set_inner_size dispatch. Replaces (supersedes) any active one.
  Dispatch {
    client: openharmony_ability_plugin_window::WindowClient,
    /// Outer width to set (width is never decor-compensated).
    w: i64,
    /// Requested INNER height (physical px) — the correction target.
    req_h: i64,
    /// Outer height dispatched (req_h + decor estimate at dispatch time).
    outer_h: i64,
    /// Window height BEFORE this dispatch (to detect "resize not landed yet").
    pre_h: i64,
    /// Decor estimate used for this dispatch.
    decor_used: i32,
  },
  /// The cached main-window decor changed (app decor_change_callback).
  Decor(i32),
  /// Delayed recheck: our own resize had not landed when the Decor event
  /// arrived (audit P1-B) — re-evaluate with a fresh decor read.
  Recheck,
}

/// Registered handle of a window's decor watcher (one per window, created
/// lazily by the first correctable set_inner_size call).
struct DecorWatchHandle {
  tx: tokio::sync::mpsc::UnboundedSender<DecorWatchMsg>,
  /// Id in openharmony-ability's decor_change_callbacks registry.
  cb_id: u64,
}

/// Fallback recheck budget per dispatch: one Recheck every 500ms while our
/// resize hasn't landed (pathological — normally it lands within tens of ms),
/// giving up after ~30s so a resize that never lands can't spin forever.
const ACTIVE_RESIZE_RECHECKS: u32 = 60;

/// The single in-flight resize a decor watcher is tracking.
struct ActiveResize {
  client: openharmony_ability_plugin_window::WindowClient,
  w: i64,
  req_h: i64,
  outer_h: i64,
  pre_h: i64,
  decor_used: i32,
  rechecks_left: u32,
}

/// Evaluate one decor observation (event or recheck) against the active
/// dispatch. See `run_decor_watch` for the correction/deactivation rules.
async fn process_decor_observation(
  active: &mut Option<ActiveResize>,
  app: &openharmony_ability::OpenHarmonyApp,
  window_id: i64,
  tx: &tokio::sync::mpsc::UnboundedSender<DecorWatchMsg>,
  decor_now: i32,
) {
  let Some(a) = active.as_mut() else { return };
  if decor_now == a.decor_used {
    return; // same estimate — nothing to correct
  }
  if decor_now < a.decor_used {
    // Downward: runtime menubar hide (146 → 66) or equivalent. The inner
    // area grows by itself; correcting would shrink the outer frame. (The
    // content area ends up LARGER than req_h by the decor delta — the
    // expected effect of hiding the menubar.) Treat as "layout intent
    // settled" and drop the active dispatch.
    *active = None;
    return;
  }
  let current = app.window_rect_for(window_id).height as i64;
  if current == a.pre_h {
    // Our own resize has not landed yet — schedule a delayed recheck instead
    // of misreading this as an external change (audit P1-B).
    let tx_retry = tx.clone();
    tokio::spawn(async move {
      tokio::time::sleep(std::time::Duration::from_millis(500)).await;
      let _ = tx_retry.send(DecorWatchMsg::Recheck);
    });
    return;
  }
  if current != a.outer_h {
    // Window moved on without us — don't stomp an external resize.
    *active = None;
    return;
  }
  let corrected = a.req_h.saturating_add(decor_now as i64);
  a.pre_h = a.outer_h;
  a.outer_h = corrected;
  a.decor_used = decor_now;
  a.rechecks_left = ACTIVE_RESIZE_RECHECKS;
  if let Err(e) = a.client.resize_window(window_id, a.w, corrected).await {
    log::warn!("[tao-ohos] resize_window (self-correct) failed for window {}: {:?}", window_id, e);
    *active = None;
  }
}

/// Per-window decor watcher task: consumes Dispatch/Decor/Recheck messages and,
/// while a dispatch is active and the decor estimate GROWS (startup layout
/// convergence — observed 70 → 146 on the reference device), re-dispatches the
/// corrected outer height so the requested INNER height survives.
///
/// Event-driven replacement of the former 15s polling loop — no periodic
/// timer, so arbitrarily slow cold starts (frontend loading for 20-30s) are
/// still corrected. Deactivation rules (stop correcting the active dispatch):
/// - downward decor change (menubar hidden at runtime: the content area GROWS
///   naturally; re-dispatching would wrongly shrink the outer frame);
/// - the window height was changed by anyone else (user drag / another
///   resize source) — never stomp an external resize;
/// - a newer Dispatch (supersession);
/// - recheck budget exhausted while our resize never landed (pathological).
async fn run_decor_watch(
  app: openharmony_ability::OpenHarmonyApp,
  window_id: i64,
  tx: tokio::sync::mpsc::UnboundedSender<DecorWatchMsg>,
  mut rx: tokio::sync::mpsc::UnboundedReceiver<DecorWatchMsg>,
) {
  let mut active: Option<ActiveResize> = None;
  loop {
    let Some(msg) = rx.recv().await else { break }; // window dropped — exit
    match msg {
      DecorWatchMsg::Dispatch { client, w, req_h, outer_h, pre_h, decor_used } => {
        if let Err(e) = client.resize_window(window_id, w, outer_h).await {
          log::warn!("[tao-ohos] resize_window failed for window {}: {:?}", window_id, e);
        }
        // A no-op resize (target == current height) has nothing to "land":
        // sentinel pre_h so the not-landed-yet guard never matches and the
        // first Decor event corrects normally (audit P1-A).
        let pre_h = if outer_h == pre_h { i64::MIN } else { pre_h };
        active = Some(ActiveResize {
          client,
          w,
          req_h,
          outer_h,
          pre_h,
          decor_used,
          rechecks_left: ACTIVE_RESIZE_RECHECKS,
        });
      }
      DecorWatchMsg::Decor(decor_now) => {
        process_decor_observation(&mut active, &app, window_id, &tx, decor_now).await;
      }
      DecorWatchMsg::Recheck => {
        if active.as_ref().is_some_and(|a| a.rechecks_left == 0) {
          // Our resize still hasn't landed after the full budget — give up.
          active = None;
          continue;
        }
        if let Some(a) = active.as_mut() {
          a.rechecks_left -= 1;
        }
        let decor_now = app.decor_height();
        process_decor_observation(&mut active, &app, window_id, &tx, decor_now).await;
      }
    }
  }
}

pub(crate) struct Window {
  app: OpenHarmonyApp,
  window_id: Option<i64>,
  /// Window kind (UIAbility/Float). The title-bar-height compensation in
  /// set_inner_size only applies to UIAbility — app.window_rect()/content_rect()
  /// is a single Rect on the shared OpenHarmonyApp, reflecting only the main
  /// window; Float sub-windows have no system title bar (FloatPage ships its own
  /// UI title bar), so applying the main window's decor_height would mismatch,
  /// hence Float skips the compensation (G7).
  kind: OHOSWindowKind,
  /// Bridge facade for async window operations (None when bridge is not ready).
  window_client: Option<openharmony_ability_plugin_window::WindowClient>,
  /// Background runtime handle for spawning async bridge calls.
  runtime: BridgeExecutor,
  /// Lazily-created per-window decor watcher (see `run_decor_watch`): one
  /// long-lived task + one decor-change callback per window, driven by events
  /// instead of a timer. Mutex<Option<..>> because set_inner_size takes &self.
  decor_watch: Arc<Mutex<Option<DecorWatchHandle>>>,
  /// State mirror for is_maximized() — written by setter intent AND backfilled
  /// by apply_window_status (windowStatusChange events), so it reflects the last
  /// known system truth (the bridge facade has no synchronous system query).
  maximized: AtomicBool,
  /// State mirror for is_minimized() — same backfill contract as `maximized`.
  minimized: AtomicBool,
  /// Phase 2: window decoration state (title bar visibility).
  /// AtomicBool supports runtime toggle from arbitrary threads.
  decorations: AtomicBool,
  /// Phase 3: whether window was created with transparent=true.
  /// Immutable after construction — set_background_color is a no-op when true.
  transparent: bool,
  // Mirror-bit sync state (issue 5):
  //   - maximized/minimized: local mirror, set_* writes intent + apply_window_status
  //     backfills system truth (the facade has no sync query; mirror + backfill is
  //     the only viable read source for sync is_*).
  //   - visible/fullscreen: local mirror, backfilled by windowStatusChange events
  //     (apply_window_status, wry drain route). set_* writes intent, events write truth.
  //   - decorations/decoration_flags: app-owned local mirror, no system state to
  //     backfill (records app intent only; related to issue 4 semantic mismatch,
  //     out of scope for this fix).
  //   - always_on_top: pure intent flag, OHOS has no z-order API (does not reflect
  //     the real system z-order).
  //   - theme: per-window field removed, now reads global APP_THEME_OVERRIDE +
  //     app.config() colorMode (continuously refreshed by ConfigChanged).
  //   See doc/OHOS-window-residual-issues.md (issue 5, 5.1/5.2/5.3).
  /// Window state mirror. visible/fullscreen are maintained by windowStatusChange backfill.
  /// Defaults: visible=true, fullscreen=false.
  visible: AtomicBool,
  fullscreen: AtomicBool,
  /// always_on_top intent flag (OHOS has no direct API; records intent only, see set_always_on_top).
  always_on_top: AtomicBool,
  /// Decoration button availability bitfield. bit0 closable, bit1 maximizable,
  /// bit2 minimizable, bit3 resizable. Defaults to 0b1111=15 (all enabled).
  decoration_flags: AtomicU8,
  /// Window size constraint cache (min/max w/h, px). OHOS `setWindowLimits`
  /// writes all four values at once (0 = unlimited), non-incrementally; so
  /// `set_min_inner_size`/`set_max_inner_size` must send both constraint sets
  /// together, otherwise the later call resets the other dimension to 0 (losing
  /// the constraint). Each setter updates its own cache slots, then reads the
  /// other's cache and sends all four. AtomicU32 because setters may be called
  /// from any thread.
  min_inner_width: AtomicU32,
  min_inner_height: AtomicU32,
  max_inner_width: AtomicU32,
  max_inner_height: AtomicU32,
}

// Upstream PR#20 window-type constants (ArkTS WindowType). Only TypeFloat is
// constructed today — the UIAbility main window needs no window_type, and the
// multi-UIAbility (TypeMain) path is not ported — but the full mapping is kept
// for parity with upstream.
#[allow(dead_code)]
enum OHOSWindowType {
  TypeApp = 0,
  TypeSystemAlert = 1,
  TypeFloat = 8,
  TypeDialog = 16,
  TypeMain = 32
}

/// OHOS `WindowStatusType` (API 11+) — the system's window mode, reported via
/// `window.on('windowStatusChange')`. Values match the ArkTS enum order:
/// 1=FULL_SCREEN, 2=MAXIMIZE, 3=MINIMIZE, 4=FLOATING, 5=SPLIT_SCREEN.
///
/// Used by [`Window::apply_window_status`] to backfill system truth into tao mirror
/// bits. See doc/OHOS-window-residual-issues.md (issue 5, 5.3).
enum WindowStatus {
  FullScreen,
  Maximize,
  Minimize,
  Floating,
  SplitScreen,
  /// UNDEFINED(0) or any unrecognized value.
  Other,
}

impl From<i32> for WindowStatus {
  fn from(value: i32) -> Self {
    match value {
      1 => WindowStatus::FullScreen,
      2 => WindowStatus::Maximize,
      3 => WindowStatus::Minimize,
      4 => WindowStatus::Floating,
      5 => WindowStatus::SplitScreen,
      _ => WindowStatus::Other,
    }
  }
}

/// Maps tao `CursorIcon` to OHOS `pointer.PointerStyle` enum value.
///
/// OHOS PointerStyle declaration order (see `@ohos.multimodalInput.pointer`):
/// DEFAULT=0, EAST=1, WEST=2, SOUTH=3, NORTH=4, WEST_EAST=5, NORTH_SOUTH=6,
/// NORTH_EAST=7, NORTH_WEST=8, SOUTH_EAST=9, SOUTH_WEST=10,
/// NORTH_EAST_SOUTH_WEST=11, NORTH_WEST_SOUTH_EAST=12, CROSS=13, CURSOR_COPY=14,
/// CURSOR_FORBID=15, ..., HAND_GRABBING=17, HAND_OPEN=18, HAND_POINTING=19,
/// HELP=20, MOVE=21, ..., TEXT_CURSOR=26, ZOOM_IN=27, ZOOM_OUT=28,
/// HORIZONTAL_TEXT_CURSOR=39, LOADING=42.
fn ohos_pointer_style(icon: window::CursorIcon) -> i32 {
  match icon {
    window::CursorIcon::Default | window::CursorIcon::Arrow | window::CursorIcon::ContextMenu | window::CursorIcon::Cell => 0,
    window::CursorIcon::Crosshair => 13,
    window::CursorIcon::Hand => 19,
    window::CursorIcon::Move | window::CursorIcon::AllScroll => 21,
    window::CursorIcon::Text => 26,
    window::CursorIcon::VerticalText => 39,
    window::CursorIcon::Wait | window::CursorIcon::Progress => 42,
    window::CursorIcon::Help => 20,
    window::CursorIcon::NotAllowed | window::CursorIcon::NoDrop => 15,
    window::CursorIcon::Alias | window::CursorIcon::Copy => 14,
    window::CursorIcon::Grab => 18,
    window::CursorIcon::Grabbing => 17,
    window::CursorIcon::ZoomIn => 27,
    window::CursorIcon::ZoomOut => 28,
    window::CursorIcon::EResize => 1,
    window::CursorIcon::WResize => 2,
    window::CursorIcon::SResize => 3,
    window::CursorIcon::NResize => 4,
    window::CursorIcon::EwResize | window::CursorIcon::ColResize => 5,
    window::CursorIcon::NsResize | window::CursorIcon::RowResize => 6,
    window::CursorIcon::NeResize => 7,
    window::CursorIcon::NwResize => 8,
    window::CursorIcon::SeResize => 9,
    window::CursorIcon::SwResize => 10,
    window::CursorIcon::NeswResize => 11,
    window::CursorIcon::NwseResize => 12,
  }
}

/// Converts tao's RGBA tuple to OHOS `0xAARRGGBB` u32 format.
///
/// When `transparent` is true, returns `Some(0x00000000)` regardless of `bg`
/// (transparent takes priority over background_color, consistent with
/// Windows/macOS behavior).
///
/// Used by both `Window::new()` (creation path) and `set_background_color()`
/// (runtime path) to avoid duplicated conversion logic.
fn rgba_to_ohos_color(transparent: bool, bg: Option<window::RGBA>) -> Option<u32> {
  if transparent {
    Some(0x00000000)
  } else {
    bg.map(|(r, g, b, a)| ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32))
  }
}

impl Window {
  pub(crate) fn new<T: 'static>(
    el: &EventLoopWindowTarget<T>,
    window_attrs: window::WindowAttributes,
    pl_attrs: PlatformSpecificWindowBuilderAttributes,
  ) -> Result<Self, error::OsError> {
    // Resolve the window kind: explicit builder choice, else the first window
    // defaults to UIAbility and any later one to Float (the single-UIAbility
    // guard below still rejects a second UIAbility — the upstream
    // start_ui_ability multi-UIAbility path is not ported; local window
    // creation supports exactly one UIAbility + Float sub-windows).
    let kind = match pl_attrs.window_kind {
      Some(kind) => kind,
      None if !UIABILITY_CREATED.load(Ordering::SeqCst) => OHOSWindowKind::UIAbility,
      None => OHOSWindowKind::Float,
    };
    let is_main_window = matches!(kind, OHOSWindowKind::UIAbility);

    if is_main_window {
      if UIABILITY_CREATED.swap(true, Ordering::SeqCst) {
        log::error!("UIAbility window already exists — only one is allowed");
        return Err(os_error!(OsError));
      }
    }

    let window_type = if is_main_window {
      // UIAbility window does not need a window_type
      0
    } else {
      // Float sub-window uses TypeFloat
      OHOSWindowType::TypeFloat as i32
    };

    let window_id = if is_main_window {
      // UIAbility window: reuse the existing main window container (DefaultXComponent).
      // window_id = 0, wry takes Path 1 (WebViewBuilder).
      Some(0)
    } else {
      // Float window: create a new OS-level floating window via create_os_window.
      // window_id > 0, wry takes Path 2 (load_url).
      let label = pl_attrs
        .label
        .clone()
        .unwrap_or_else(|| window_attrs.title.clone());
      // Honor the builder's inner_size/position (logical px → physical) so a
      // Float WebviewWindow sized via `.inner_size()/.position()` actually
      // applies. Without this, createOSWindow falls back to the 800×600 default
      // and ignores the requested geometry entirely.
      let scale = el.app.scale() as f64;
      let (width, height) = window_attrs
        .inner_size
        .map(|s| {
          let p = s.to_physical::<i32>(scale);
          (p.width, p.height)
        })
        .unwrap_or((800, 600));
      let (x, y) = window_attrs
        .position
        .map(|p| {
          let phys = p.to_physical::<i32>(scale);
          (phys.x, phys.y)
        })
        .unwrap_or((100, 100));
      let params = WindowCreateParams {
        name: label.clone(),
        window_type: window_type as i32,
        width,
        height,
        x,
        y,
        decorations: window_attrs.decorations,
        transparent: window_attrs.transparent,
        background_color: rgba_to_ohos_color(
          window_attrs.transparent,
          window_attrs.background_color,
        ),
      };
      match create_os_window(params) {
        Ok(id) => Some(id),
        Err(e) => {
          log::error!("[tao-ohos] create_os_window failed for Float window {:?}: {:?}", label, e);
          return Err(os_error!(OsError));
        }
      }
    };

    // Create the WindowClient bridge facade. If the bridge runtime is not yet
    // ready (e.g. during early init), window_client = None and all window
    // operations degrade to no-ops with a warn! log.
    let window_client = el.app.window().ok();
    let runtime = el.bridge_executor.clone();

    let win = Self {
      app: el.app.clone(),
      window_id,
      kind,
      window_client,
      runtime,
      maximized: AtomicBool::new(false),
      minimized: AtomicBool::new(false),
      decor_watch: Arc::new(Mutex::new(None)),
      decorations: AtomicBool::new(window_attrs.decorations),
      transparent: window_attrs.transparent,
      visible: AtomicBool::new(true),
      fullscreen: AtomicBool::new(false),
      always_on_top: AtomicBool::new(false),
      decoration_flags: AtomicU8::new(FLAG_ALL_DECORATIONS),
      min_inner_width: AtomicU32::new(0),
      min_inner_height: AtomicU32::new(0),
      max_inner_width: AtomicU32::new(0),
      max_inner_height: AtomicU32::new(0),
    };

    // Apply decorations immediately for the main window at creation time.
    // Without this, the main window retains its default OS decorations even if
    // the builder specified .decorations(false), because Window::set_decorations()
    // is only called later (if at all) by the user.
    if is_main_window && !window_attrs.decorations {
      if let Some(ref client) = win.window_client {
        let client = client.clone();
        win.runtime.spawn(async move {
          if let Err(e) = client.set_window_decorations(0, false).await {
            log::warn!("[tao-ohos] set_window_decorations failed for window 0: {:?}", e);
          }
        });
      }
    }

    Ok(win)
  }

  pub fn request_redraw(&self) {
    // OHOS vsync auto-drives rendering; there is no app-initiated redraw API,
    // so this is a no-op (the former ArkTS bridge was removed upstream).
  }

  #[inline]
  pub fn monitor_from_point(&self, x: f64, y: f64) -> Option<monitor::MonitorHandle> {
    // OHOS is single-display; return primary when the point is within the
    // default display bounds (DisplayManager physical pixels). See ohos-monitor-real-values.
    let w = self.app.display_width() as f64;
    let h = self.app.display_height() as f64;
    if w > 0.0 && h > 0.0 && x >= 0.0 && y >= 0.0 && x < w && y < h {
      Some(monitor::MonitorHandle {
        inner: MonitorHandle::new(self.app.clone()),
      })
    } else {
      None
    }
  }

  pub fn id(&self) -> WindowId {
    // Phase 3 (design.md D6): return this window's own OHOS windowId instead of
    // the ZST constant, so tauri-runtime-wry's window_id_map routes events to the
    // correct WindowWrapper. Main window → WindowId(0), Float sub-window → WindowId(N).
    WindowId(self.window_id.unwrap_or(0))
  }

  pub fn scale_factor(&self) -> f64 {
    self.app.scale() as f64
  }

  pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
    let mut v = VecDeque::with_capacity(1);
    v.push_back(MonitorHandle::new(self.app.clone()));
    v
  }

  pub fn inner_position(&self) -> Result<PhysicalPosition<i32>, error::NotSupportedError> {
    let content = self.app.content_rect();
    // Phase 2 (design.md D5): read this window's own rect by window_id instead of the
    // shared single field, so sub-windows don't read the main window's rect.
    let window = self.app.window_rect_for(self.window_id.unwrap_or(0));
    // inner_position = content area position on screen
    // = window position + system title-bar offset + content offset within container.
    // content_rect.left/top is XComponent offset relative to its parent container,
    // which already sits BELOW the system title bar — the title bar height is not
    // included. Add decor_height, mirroring set_inner_size's compensation (issue 2
    // getter side): without this, innerPosition == outerPosition on decorated windows
    // (observed 2026-08-20: inner (515,451) == outer (515,451) with a 146px title
    // bar; true content origin is (515,597)).
    // Float sub-windows have no system title bar (FloatPage ships its own UI bar):
    // skip, same as set_inner_size. (G7: the mirrored rects track the MAIN window,
    // so this getter is only meaningful for main/UIAbility windows regardless.)
    //
    // decor_height reads the CACHED estimate (app.decor_height()), not a live
    // window_rect − content_rect diff: the WM rect and the XComponent surface rect
    // update asynchronously, and a read in the gap between them produces garbage
    // (observed 824/770/292 instead of the real 146). The cache is latched on
    // surface events only, where both rects are consistent.
    let decor_height = if self.kind == OHOSWindowKind::Float {
      0
    } else {
      self.app.decor_height()
    };
    Ok(PhysicalPosition::new(
      window.left + content.left,
      window.top + content.top + decor_height,
    ))
  }

  pub fn inner_size(&self) -> PhysicalSize<u32> {
    // D2 hybrid (design.md): OHOS win.resize() sets the OUTER size (including
    // title bar). inner_size must mirror set_inner_size's compensation so
    // save→restore cycles are idempotent:
    //   save inner_size (= outer − decor) → restore resize(inner + decor) = outer.
    // Returning the raw outer rect here (the pre-D2 behavior) made every
    // window-state save→restore round grow the window by one title bar.
    //
    // Float sub-windows have no system title bar (FloatPage ships its own UI
    // bar): decor = 0, inner == outer. content_rect() mirrors the MAIN window
    // (G7), which is fine — the system title bar height is uniform app-wide,
    // and this getter is only meaningful for UIAbility windows regardless.
    // Web content sizing is unaffected: the Web component uses natural layout
    // ("100%"), so it never reads inner_size.
    //
    // decor_height reads the CACHED estimate (app.decor_height()), latched on
    // surface events. The previous live window_rect − content_rect diff raced:
    // the WM rect updates ~10-40ms before the XComponent surface rect, and an
    // inner_size() call in that gap computed garbage decor (824/770/292 instead
    // of 146), corrupting reads that tests then fed back through setSize —
    // the root cause of the shrinking-main-window bug.
    let rect = self.app.window_rect_for(self.window_id.unwrap_or(0));
    let decor_height = if self.kind == OHOSWindowKind::Float {
      0
    } else {
      self.app.decor_height().max(0) as u32
    };
    let inner_height = (rect.height as u32).saturating_sub(decor_height);
    PhysicalSize::new(rect.width as _, inner_height)
  }

  pub fn set_inner_size(&self, size: Size) {
    // Guard: when FLAG_RESIZABLE is 0, disallow resize (issue 4: semantic mismatch fix)
    if (self.decoration_flags.load(Ordering::Acquire) & FLAG_RESIZABLE) == 0 {
      log::warn!("[tao-ohos] set_inner_size blocked: FLAG_RESIZABLE not set");
      return;
    }
    // Compensate for title bar height: OHOS win.resize() sets the OUTER size
    // (including title bar), but the caller expects INNER size (content area).
    // decor_height = main-window title bar inset (cached, latched on surface
    // events — see app.rs latch_decor_height).
    // Without this, save→restore loops shrink the window by one title bar each cycle
    // (issue 2: inner/outer semantic mismatch). Width is NOT compensated (title bar only
    // affects height).
    //
    // G7: content_rect() reads a single Rect on the shared OpenHarmonyApp that
    // mirrors the MAIN UIAbility window — not this window. The compensation is
    // therefore only valid for UIAbility windows: the system title bar height
    // is uniform app-wide, so the main window's decor_height is a correct
    // approximation even for subsequent UIAbilities. Float sub-windows have
    // no system title bar (FloatPage ships its own UI title bar), so applying
    // the main window's decor_height would shrink/grow them by the wrong
    // inset — skip. The window rect itself is per-window (window_rect_for).
    //
    // decor_height reads the CACHED estimate (app.decor_height()) — same race
    // rationale as inner_size: a live window_rect − content_rect diff in the
    // WM-rect/surface-rect update gap produced garbage decor, which fed back
    // through resize(inner + garbage) and compounded the shrink.
    let is_float = self.kind == OHOSWindowKind::Float;
    let decor_height = if is_float { 0 } else { self.app.decor_height().max(0) as u32 };
    // For LogicalSize, convert via the real scale_factor (a hardcoded 1.0 would
    // halve the window on DPR≠1 displays). The ArkTS side
    // (WindowManager.resizeWindow) does NOT compensate — it calls win.resize(w, h)
    // directly, so the value dispatched here is the outer size.
    let s = size.to_physical::<u32>(self.scale_factor());
    let outer_height = s.height.saturating_add(decor_height);
    if let Some(window_id) = self.window_id {
      let client = match &self.window_client {
        Some(c) => c.clone(),
        None => return,
      };
      let w = s.width as i64;
      let h = outer_height as i64;
      if is_float {
        // Float sub-windows: decor is 0 by design (FloatPage ships its own UI
        // bar) and app.decor_height() mirrors the MAIN window — a watcher fed
        // by main-window decor events would mis-correct Float windows by the
        // main window's title-bar inset (observed 1520x1140 → 1520x1286).
        // Fire-and-forget, no self-correction.
        self.runtime.spawn(async move {
          if let Err(e) = client.resize_window(window_id, w, h).await {
            log::warn!("[tao-ohos] resize_window failed for window {}: {:?}", window_id, e);
          }
        });
        return;
      }
      // UIAbility window: route through the per-window decor watcher so a
      // dispatch that raced a transient decor estimate (notably window-state
      // restore on startup, where layout converges to the real decor only
      // after the webview frontend loads) is self-corrected when the cached
      // decor converges. See run_decor_watch for the correction/deactivation
      // rules.
      let pre_h = self.app.window_rect_for(window_id).height as i64;
      match self.ensure_decor_watch(window_id) {
        Some(tx) => {
          let _ = tx.send(DecorWatchMsg::Dispatch {
            client,
            w,
            req_h: s.height as i64,
            outer_h: h,
            pre_h,
            decor_used: decor_height as i32,
          });
        }
        None => {
          // Watcher unavailable (registration failed) — plain dispatch.
          self.runtime.spawn(async move {
            if let Err(e) = client.resize_window(window_id, w, h).await {
              log::warn!("[tao-ohos] resize_window failed for window {}: {:?}", window_id, e);
            }
          });
        }
      }
    }
  }

  /// Lazily create this window's decor watcher (task + decor-change callback)
  /// on the first correctable set_inner_size call, and return its sender.
  /// Later calls reuse the existing watcher — one task and one callback per
  /// window for the window's lifetime, so no per-dispatch accumulation.
  /// Returns None only when callback registration failed (app RwLock
  /// poisoned — post-panic only); callers degrade to fire-and-forget.
  fn ensure_decor_watch(
    &self,
    window_id: i64,
  ) -> Option<tokio::sync::mpsc::UnboundedSender<DecorWatchMsg>> {
    let mut guard = self.decor_watch.lock().expect("decor_watch poisoned");
    if let Some(handle) = guard.as_ref() {
      return Some(handle.tx.clone());
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    // The callback runs under the app's RwLock write lock — it must stay
    // lock-free. An unbounded-channel send is exactly that (non-blocking).
    let tx_for_cb = tx.clone();
    let cb_id = self.app.register_decor_change_callback(Arc::new(move |decor: i32| {
      let _ = tx_for_cb.send(DecorWatchMsg::Decor(decor));
      true // keep registered; removed when the window is dropped
    }));
    if cb_id == u64::MAX {
      // Registration failed (poisoned lock): no callback → the watcher would
      // never observe decor changes. Don't spawn it.
      return None;
    }
    let app = self.app.clone();
    let tx_for_watch = tx.clone();
    self.runtime.spawn(async move {
      run_decor_watch(app, window_id, tx_for_watch, rx).await;
    });
    *guard = Some(DecorWatchHandle { tx: tx.clone(), cb_id });
    Some(tx)
  }

  pub fn set_inner_size_constraints(&self, _: WindowSizeConstraints) {}

  pub fn outer_position(&self) -> Result<PhysicalPosition<i32>, error::NotSupportedError> {
    let rect = self.app.window_rect_for(self.window_id.unwrap_or(0));
    Ok(PhysicalPosition::new(rect.left, rect.top))
  }

  pub fn set_outer_position(&self, position: Position) {
    if let Some(window_id) = self.window_id {
      let physical = position.to_physical::<i32>(self.scale_factor());
      let client = match &self.window_client {
        Some(c) => c.clone(),
        None => return,
      };
      let x = physical.x as i64;
      let y = physical.y as i64;
      self.runtime.spawn(async move {
        if let Err(e) = client.move_window_to(window_id, x, y).await {
          log::warn!("[tao-ohos] move_window_to failed for window {}: {:?}", window_id, e);
        }
      });
    }
  }

  pub fn outer_size(&self) -> PhysicalSize<u32> {
    let window = self.app.window_rect_for(self.window_id.unwrap_or(0));
    // window_rect is set by ArkTS callback, may be (0,0,0,0) initially
    // fallback to content_rect if not yet initialized
    if window.width > 0 && window.height > 0 {
      PhysicalSize::new(window.width as _, window.height as _)
    } else {
      let content = self.app.content_rect();
      PhysicalSize::new(content.width as _, content.height as _)
    }
  }

  pub fn set_min_inner_size(&self, size: Option<Size>) {
    // OHOS setWindowLimits (API 11+) writes all four (min/max w/h) at once, where
    // 0 = no limit. It is NOT incremental — a call sets the whole tuple. So we cache
    // the min here, read the cached max, and dispatch both constraints together.
    // Otherwise calling set_min after set_max would reset max to 0 (dropping the max
    // constraint), and vice versa — the two setters were mutually exclusive.
    // ⚠️ Triggers OnSizeChange — do not call frequently (appfreeze risk).
    let (min_w, min_h) = match size {
      Some(s) => {
        let p = s.to_physical::<u32>(self.scale_factor());
        (p.width, p.height)
      }
      None => (0, 0),
    };
    self.min_inner_width.store(min_w, Ordering::Release);
    self.min_inner_height.store(min_h, Ordering::Release);
    let max_w = self.max_inner_width.load(Ordering::Acquire);
    let max_h = self.max_inner_height.load(Ordering::Acquire);
    if let Some(window_id) = self.window_id {
      let client = match &self.window_client {
        Some(c) => c.clone(),
        None => return,
      };
      self.runtime.spawn(async move {
        if let Err(e) = client.set_window_limits(window_id, min_w as i64, min_h as i64, max_w as i64, max_h as i64).await {
          log::warn!("[tao-ohos] set_window_limits (min) failed for window {}: {:?}", window_id, e);
        }
      });
    }
  }

  pub fn set_max_inner_size(&self, size: Option<Size>) {
    // See set_min_inner_size note. Cache max, read cached min, dispatch both together.
    let (max_w, max_h) = match size {
      Some(s) => {
        let p = s.to_physical::<u32>(self.scale_factor());
        (p.width, p.height)
      }
      None => (0, 0),
    };
    self.max_inner_width.store(max_w, Ordering::Release);
    self.max_inner_height.store(max_h, Ordering::Release);
    let min_w = self.min_inner_width.load(Ordering::Acquire);
    let min_h = self.min_inner_height.load(Ordering::Acquire);
    if let Some(window_id) = self.window_id {
      let client = match &self.window_client {
        Some(c) => c.clone(),
        None => return,
      };
      self.runtime.spawn(async move {
        if let Err(e) = client.set_window_limits(window_id, min_w as i64, min_h as i64, max_w as i64, max_h as i64).await {
          log::warn!("[tao-ohos] set_window_limits (max) failed for window {}: {:?}", window_id, e);
        }
      });
    }
  }

  pub fn set_title(&self, title: &str) {
    // OHOS setWindowTitle (API 9+, callback form). Main window + Float sub-windows
    // both support title text. Only visible when decorations enabled (decorEnabled=true).
    // Icon is NOT changeable at runtime.
    if let Some(window_id) = self.window_id {
      let client = match &self.window_client {
        Some(c) => c.clone(),
        None => return,
      };
      let title = title.to_string();
      self.runtime.spawn(async move {
        if let Err(e) = client.set_window_title(window_id, title).await {
          log::warn!("[tao-ohos] set_window_title failed for window {}: {:?}", window_id, e);
        }
      });
    }
  }

  pub fn set_visible(&self, visibility: bool) {
    // window_id 0 (main window) is valid for minimize/restore/show/move/resize/maximize
    // (unlike set_focus/set_focusable, where the main window is OS-managed and guarded
    // with `window_id > 0`), so no guard here — programmatic minimize on the main window
    // works (verified on device).
    //
    // OHOS has no direct window-hide API, so set_visible(false) uses minimize as a
    // workaround. Since is_minimized() reads the local AtomicBool mirror (not
    // getWindowStatus()), we sync the mirror here — the same pattern as
    // set_minimized() — so is_minimized() stays consistent with the visible state.
    // set_visible(true) uses restore (API14) + show_window; on API12 restore is
    // unavailable → show_window best-effort (may not restore a minimized main
    // window). The mirror is cleared regardless, matching the restore intent.
    if let Some(window_id) = self.window_id {
      let client = match &self.window_client {
        Some(c) => c.clone(),
        None => return,
      };
      if visibility {
        self.minimized.store(false, Ordering::Release);
        // TODO(A1): replace with AppControlExt::show_ability(env) when A1 adds the action
        self.runtime.spawn(async move {
          if let Err(e) = client.restore_window(window_id).await {
            log::warn!("[tao-ohos] restore_window failed for window {}: {:?}", window_id, e);
          }
          if let Err(e) = client.show_window(window_id).await {
            log::warn!("[tao-ohos] show_window failed for window {}: {:?}", window_id, e);
          }
        });
      } else {
        self.minimized.store(true, Ordering::Release);
        // TODO(A1): replace with AppControlExt::hide_ability(env) when A1 adds the action
        self.runtime.spawn(async move {
          if let Err(e) = client.minimize_window(window_id).await {
            log::warn!("[tao-ohos] minimize_window failed for window {}: {:?}", window_id, e);
          }
        });
      }
    }
  }

  pub fn set_focus(&self) {
    if let Some(window_id) = self.window_id {
      if window_id > 0 {
        let client = match &self.window_client {
          Some(c) => c.clone(),
          None => return,
        };
        self.runtime.spawn(async move {
          if let Err(e) = client.focus_window(window_id).await {
            log::warn!(
              "set_focus: focus_window failed for window {}: {:?}",
              window_id, e
            );
          }
        });
      }
      // Main window (window_id = 0): focus is OS-managed, no-op
    }
  }

  pub fn set_focusable(&self, focusable: bool) {
    if let Some(window_id) = self.window_id {
      if window_id > 0 {
        let client = match &self.window_client {
          Some(c) => c.clone(),
          None => return,
        };
        self.runtime.spawn(async move {
          if let Err(e) = client.set_window_focusable(window_id, focusable).await {
            log::warn!(
              "set_focusable: set_window_focusable failed for window {}: {:?}",
              window_id, e
            );
          }
        });
      }
      // Main window (window_id = 0): focusable is OS-managed, no-op
    }
  }

  pub fn is_focused(&self) -> bool {
    HAS_FOCUS.load(Ordering::Relaxed)
  }

  pub fn is_always_on_top(&self) -> bool {
    // Intent flag only — OHOS has no z-order query API (see set_always_on_top).
    self.always_on_top.load(Ordering::Acquire)
  }

  // TODO(issue 4): set_resizable/set_minimizable/set_maximizable/set_closable
  //   nominally control "whether the window can resize/minimize/maximize/close",
  //   but set_decoration_flag only toggles decoration button visibility
  //   (FloatPage @LocalStorageProp); it does not block programmatic APIs like
  //   set_minimized/set_maximized/close/set_inner_size. is_resizable etc. also read
  //   from the local mirror, returning a false promise. The main window is a
  //   complete no-op. See doc/OHOS-window-residual-issues.md (issue 4).
  pub fn set_resizable(&self, resizable: bool) {
    self.set_decoration_flag(FLAG_RESIZABLE, resizable);
  }

  pub fn set_minimizable(&self, minimizable: bool) {
    self.set_decoration_flag(FLAG_MINIMIZABLE, minimizable);
  }

  pub fn set_maximizable(&self, maximizable: bool) {
    self.set_decoration_flag(FLAG_MAXIMIZABLE, maximizable);
  }

  pub fn set_closable(&self, closable: bool) {
    self.set_decoration_flag(FLAG_CLOSABLE, closable);
  }

  /// Common helper: update one decoration bit and dispatch to ArkTS (FloatPage LocalStorage).
  /// Dispatched via the window bridge facade fire-and-forget (no `set-decorations`
  /// variant carrying flags exists — the ArkTS WindowManager.setDecorationFlag
  /// intercepts by reading this bitfield; here we only write the local mirror +
  /// log, dispatching via the equivalent `set_window_decoration_flags` action).
  fn set_decoration_flag(&self, flag: u8, on: bool) {
    let mut flags = self.decoration_flags.load(Ordering::Acquire);
    if on { flags |= flag; } else { flags &= !flag; }
    self.decoration_flags.store(flags, Ordering::Release);
    if let Some(window_id) = self.window_id {
      let client = match &self.window_client {
        Some(c) => c.clone(),
        None => return,
      };
      self.runtime.spawn(async move {
        if let Err(e) = client.set_window_decoration_flags(window_id, flags as i32).await {
          log::warn!("[tao-ohos] set_window_decoration_flags failed for window {}: {:?}", window_id, e);
        }
      });
    }
  }

  pub fn set_minimized(&self, minimized: bool) {
    // Guard: when FLAG_MINIMIZABLE is 0, disallow minimize (issue 4: semantic mismatch fix)
    if minimized && (self.decoration_flags.load(Ordering::Acquire) & FLAG_MINIMIZABLE) == 0 {
      log::warn!("[tao-ohos] set_minimized(true) blocked: FLAG_MINIMIZABLE not set");
      return;
    }
    // Update the mirror synchronously (setter intent); apply_window_status
    // backfills the system truth when the windowStatusChange event arrives.
    self.minimized.store(minimized, Ordering::Release);
    if let Some(window_id) = self.window_id {
      let client = match &self.window_client {
        Some(c) => c.clone(),
        None => return,
      };
      if minimized {
        self.runtime.spawn(async move {
          if let Err(e) = client.minimize_window(window_id).await {
            log::warn!("[tao-ohos] minimize_window failed for window {}: {:?}", window_id, e);
          }
        });
      } else {
        self.runtime.spawn(async move {
          if let Err(e) = client.restore_window(window_id).await {
            log::warn!("[tao-ohos] restore_window failed for window {}: {:?}", window_id, e);
          }
        });
      }
    }
  }

  pub fn is_minimized(&self) -> bool {
    self.minimized.load(Ordering::Acquire)
  }

  pub fn set_maximized(&self, maximized: bool) {
    // Guard: when FLAG_MAXIMIZABLE is 0, disallow maximize (issue 4: semantic mismatch fix)
    if maximized && (self.decoration_flags.load(Ordering::Acquire) & FLAG_MAXIMIZABLE) == 0 {
      log::warn!("[tao-ohos] set_maximized(true) blocked: FLAG_MAXIMIZABLE not set");
      return;
    }
    // Update the mirror synchronously (setter intent); apply_window_status
    // backfills the system truth when the windowStatusChange event arrives.
    self.maximized.store(maximized, Ordering::Release);
    if let Some(window_id) = self.window_id {
      let client = match &self.window_client {
        Some(c) => c.clone(),
        None => return,
      };
      if maximized {
        self.runtime.spawn(async move {
          if let Err(e) = client.maximize_window(window_id).await {
            log::warn!("[tao-ohos] maximize_window failed for window {}: {:?}", window_id, e);
          }
        });
      } else {
        // recover() switches MAXIMIZE/FULL_SCREEN → FLOATING (API7+, public)
        self.runtime.spawn(async move {
          if let Err(e) = client.recover_window(window_id).await {
            log::warn!("[tao-ohos] recover_window failed for window {}: {:?}", window_id, e);
          }
        });
      }
    }
  }

  pub fn is_maximized(&self) -> bool {
    self.maximized.load(Ordering::Acquire)
  }

  pub fn set_fullscreen(&self, monitor: Option<Fullscreen>) {
    // Delegate to the WindowClient bridge facade (plugin-window). `on=true`
    // enters an immersive fullscreen (setWindowLayoutFullScreen(true) + hide
    // system bars); `on=false` reverses it. Dispatched via `runtime.spawn` —
    // fire-and-forget at the JS level (the ArkTS handler returns after kicking
    // off async Promises), so it does not block the main thread. Replaces the
    // legacy synchronous `set_fullscreen` NAPI call which went through the dead
    // `get_helper()` transport.
    let on = monitor.is_some();
    // Sync the fullscreen mirror (read by `fullscreen()` for sync is_fullscreen
    // queries; event backfill via apply_window_status corrects it if the
    // dispatch fails). Also sync the maximized cache: fullscreen implies
    // maximized (entering fullscreen is effectively maximize + immersive),
    // exiting fullscreen calls recover() which un-maximizes. Without this,
    // is_maximized() returns stale state after a fullscreen toggle, causing
    // the next maximize/unmaximize to be a no-op.
    self.fullscreen.store(on, Ordering::Release);
    self.maximized.store(on, Ordering::Release);
    if let Some(window_id) = self.window_id {
      let client = match &self.window_client {
        Some(c) => c.clone(),
        None => return,
      };
      self.runtime.spawn(async move {
        if let Err(e) = client.set_fullscreen(window_id, on).await {
          log::warn!(
            "[tao-ohos] set_fullscreen failed for window {}: {:?}",
            window_id,
            e
          );
        }
      });
    }
  }

  pub fn fullscreen(&self) -> Option<Fullscreen> {
    // OHOS fullscreen is an immersive layout mode, not a monitor-bound
    // Fullscreen::Exclusive/Borderless(MonitorHandle) state — report the
    // mirror bit (written by set_fullscreen and backfilled from
    // windowStatusChange events via apply_window_status) as Borderless(None),
    // matching upstream. Returning None unconditionally made is_fullscreen()
    // always false, so a fullscreen toggle could enter but never exit.
    if self.fullscreen.load(Ordering::Acquire) {
      Some(Fullscreen::Borderless(None))
    } else {
      None
    }
  }

  /// Backfills system window status into tao mirror bits (issue 5, 5.3).
  ///
  /// Called by tauri-runtime-wry's OHOS drain block: the `windowStatusChange`
  /// event is enqueued via the `notify_window_status` NAPI, then after draining
  /// it is routed to this `Window` by the real OHOS windowId, writing the system
  /// truth into the mirror bits.
  ///
  /// `maximized`/`minimized` are likewise backfilled via the mirror: the bridge
  /// facade has no synchronous system query (the old framework's `getWindowStatus()`
  /// sync NAPI was removed with the ArkHelper channel), so event backfill +
  /// setter intent writes are the only viable read source for sync
  /// `is_maximized()`/`is_minimized()`.
  ///
  /// `status` is a raw OHOS `WindowStatusType` value (passed through from ArkTS).
  pub fn apply_window_status(&self, status: i32) {
    match WindowStatus::from(status) {
      WindowStatus::FullScreen => {
        // System fullscreen: visible + fullscreen + maximized (tauri's fullscreen entry path mirrors this synchronously).
        self.visible.store(true, Ordering::Release);
        self.fullscreen.store(true, Ordering::Release);
        self.maximized.store(true, Ordering::Release);
        self.minimized.store(false, Ordering::Release);
      }
      WindowStatus::Maximize => {
        // Maximize: visible, not fullscreen, not minimized.
        self.visible.store(true, Ordering::Release);
        self.fullscreen.store(false, Ordering::Release);
        self.maximized.store(true, Ordering::Release);
        self.minimized.store(false, Ordering::Release);
      }
      WindowStatus::Minimize => {
        // Minimize: not visible, not fullscreen, not maximized.
        self.visible.store(false, Ordering::Release);
        self.fullscreen.store(false, Ordering::Release);
        self.maximized.store(false, Ordering::Release);
        self.minimized.store(true, Ordering::Release);
      }
      WindowStatus::Floating => {
        // Free floating (normal): visible, not fullscreen, not maximized, not minimized.
        self.visible.store(true, Ordering::Release);
        self.fullscreen.store(false, Ordering::Release);
        self.maximized.store(false, Ordering::Release);
        self.minimized.store(false, Ordering::Release);
      }
      WindowStatus::SplitScreen => {
        // Split screen: visible, not fullscreen (tao has no split-screen concept,
        // treat as visible); maximized left untouched — a split half is neither
        // maximized nor floating, cannot be reliably inferred.
        self.visible.store(true, Ordering::Release);
        self.fullscreen.store(false, Ordering::Release);
      }
      WindowStatus::Other => {
        // UNDEFINED/unknown value: don't change anything, avoid accidental clearing.
      }
    }
  }
  pub fn set_decorations(&self, decorations: bool) {
    self.decorations.store(decorations, Ordering::Release);
    if let Some(window_id) = self.window_id {
      let client = match &self.window_client {
        Some(c) => c.clone(),
        None => return,
      };
      self.runtime.spawn(async move {
        if let Err(e) = client.set_window_decorations(window_id, decorations).await {
          log::warn!("[tao-ohos] set_window_decorations failed for window {}: {:?}", window_id, e);
        }
      });
    }
  }
  pub fn set_always_on_bottom(&self, _always_on_bottom: bool) {}

  pub fn set_always_on_top(&self, always_on_top: bool) {
    // Records intent (is_always_on_top reads this) AND dispatches to OHOS
    // setWindowTopmost (API 14+, needs ohos.permission.WINDOW_TOPMOST) via the
    // window bridge facade. Main window only per OHOS docs; Float sub-windows
    // will error (caught + warned in ArkTS, non-fatal). Only effective in
    // freeform window mode.
    self.always_on_top.store(always_on_top, Ordering::Release);
    if let Some(window_id) = self.window_id {
      let client = match &self.window_client {
        Some(c) => c.clone(),
        None => return,
      };
      self.runtime.spawn(async move {
        if let Err(e) = client.set_window_topmost(window_id, always_on_top).await {
          log::warn!("[tao-ohos] set_window_topmost failed for window {}: {:?}", window_id, e);
        }
      });
    }
  }
  pub fn set_ime_position(&self, position: Position) {
    // IME position: convert to physical pixels and forward to ArkTS
    // inputMethod.getController().updateCursor(CursorInfo).
    // Prerequisite: a focused edit field inside the window (an HTML input works),
    // otherwise error 12800009 client detached is returned (expected/normal).
    // Verified OK after focusing an HTML input (2026-08-19) — works for the
    // webview scenario, not an architectural limitation.
    let p = position.to_physical::<i32>(self.scale_factor());
    if let Some(window_id) = self.window_id {
      let client = match &self.window_client {
        Some(c) => c.clone(),
        None => return,
      };
      let x = p.x as i64;
      let y = p.y as i64;
      self.runtime.spawn(async move {
        if let Err(e) = client.set_ime_position(window_id, x, y).await {
          log::warn!("[tao-ohos] set_ime_position failed for window {}: {:?}", window_id, e);
        }
      });
    }
  }

  pub fn is_decorated(&self) -> bool {
    self.decorations.load(Ordering::Acquire)
  }

  pub fn is_visible(&self) -> bool {
    self.visible.load(Ordering::Acquire)
  }

  pub fn is_resizable(&self) -> bool {
    self.decoration_flags.load(Ordering::Acquire) & FLAG_RESIZABLE != 0
  }

  pub fn is_minimizable(&self) -> bool {
    self.decoration_flags.load(Ordering::Acquire) & FLAG_MINIMIZABLE != 0
  }

  pub fn is_maximizable(&self) -> bool {
    self.decoration_flags.load(Ordering::Acquire) & FLAG_MAXIMIZABLE != 0
  }

  pub fn is_closable(&self) -> bool {
    self.decoration_flags.load(Ordering::Acquire) & FLAG_CLOSABLE != 0
  }

  pub fn set_window_icon(&self, _window_icon: Option<crate::icon::Icon>) {}

  pub fn set_cursor_icon(&self, icon: window::CursorIcon) {
    // TODO(issue 6): dispatched but not yet device-tested — verify style mapping
    //   coverage, touch-mode device behavior, and whether it works on Float
    //   sub-windows. See doc/OHOS-window-residual-issues.md (issue 6).
    // Set cursor style by windowId (pointer.setPointerStyleSync), dispatched via
    // the window bridge facade fire-and-forget (ArkTS side delegates to
    // WindowManager.setPointerStyle, using the real OHOS window id).
    let style = ohos_pointer_style(icon);
    if let Some(window_id) = self.window_id {
      let client = match &self.window_client {
        Some(c) => c.clone(),
        None => return,
      };
      self.runtime.spawn(async move {
        if let Err(e) = client.set_cursor_icon(window_id, style).await {
          log::warn!("[tao-ohos] set_cursor_icon failed for window {}: {:?}", window_id, e);
        }
      });
    }
  }
  pub fn set_cursor_grab(&self, grab: bool) -> Result<(), error::ExternalError> {
    // OH_WindowManager_LockCursor/UnlockCursor (NDK C API 22+, resolved via
    // dlopen in openharmony-ability). Lock is confined-follow mode — cursor
    // keeps moving within the window area, matching Windows ClipCursor
    // semantics. Only effective while the window is focused; the system
    // releases the lock automatically on focus loss (platform difference vs
    // Windows — apps that need a persistent lock re-grab on Focused(true)).
    //
    // D3.7 two-phase: the FFI needs the REAL OHOS window id
    // (getWindowProperties().id), resolved through the window bridge facade
    // (`get-real-window-id` action) because the ability crate cannot reach the
    // plugin-window facade (dependency direction plugin-window → ability).
    // Phase 1 (sync): reject API < 22 with NotSupported — same error callers
    // saw before the feature existed. Phase 2 (async): resolve the real id and
    // invoke the FFI fire-and-forget on the bridge runtime.
    if openharmony_ability::sdk_api_version() < 22 {
      return Err(error::ExternalError::NotSupported(
        error::NotSupportedError::new(),
      ));
    }
    let window_id = self.window_id.ok_or_else(|| {
      error::ExternalError::NotSupported(error::NotSupportedError::new())
    })?;
    let client = match &self.window_client {
      Some(c) => c.clone(),
      None => {
        log::warn!(
          "[tao-ohos] set_cursor_grab: WindowClient not initialized for window {}",
          window_id
        );
        return Err(error::ExternalError::NotSupported(
          error::NotSupportedError::new(),
        ));
      }
    };
    self.runtime.spawn(async move {
      match client.get_real_window_id(window_id).await {
        Ok(real_id) => {
          if let Err(e) = set_cursor_grab(real_id as i32, grab) {
            log::warn!(
              "[tao-ohos] set_cursor_grab({}) failed for window {} (real id {}): {}",
              grab,
              window_id,
              real_id,
              e
            );
          }
        }
        Err(e) => {
          log::warn!(
            "[tao-ohos] set_cursor_grab({}): get_real_window_id failed for window {}: {:?}",
            grab,
            window_id,
            e
          );
        }
      }
    });
    Ok(())
  }

  pub fn request_user_attention(&self, _request_type: Option<window::UserAttentionType>) {
    // OHOS window layer has no requestAttention API. Emulated via
    // notificationManager on the ArkTS side (fire-and-forget; the plugin
    // handles the 1600004 enable-notification retry path).
    if let Some(window_id) = self.window_id {
      let client = match &self.window_client {
        Some(c) => c.clone(),
        None => return,
      };
      self.runtime.spawn(async move {
        if let Err(e) = client.request_user_attention(window_id).await {
          log::warn!("[tao-ohos] request_user_attention failed for window {}: {:?}", window_id, e);
        }
      });
    }
  }

  pub fn set_cursor_position(&self, _: Position) -> Result<(), error::ExternalError> {
    Err(error::ExternalError::NotSupported(
      error::NotSupportedError::new(),
    ))
  }

  pub fn cursor_position(&self) -> Result<PhysicalPosition<f64>, error::ExternalError> {
    // Same source as EventLoopWindowTarget::cursor_position — see the note there.
    let scale = self.app.scale() as f64;
    let x = f64::from_bits(openharmony_ability::CURSOR_POSITION_X.load(Ordering::Relaxed)) * scale;
    let y = f64::from_bits(openharmony_ability::CURSOR_POSITION_Y.load(Ordering::Relaxed)) * scale;
    Ok(PhysicalPosition::new(x, y))
  }

  pub fn set_ignore_cursor_events(&self, ignore: bool) -> Result<(), error::ExternalError> {
    // window_id is None for embedded webviews with no OS-level window — cursor-event
    // ignore is genuinely unsupported there, so surface NotSupported (per design D4).
    // Main window (window_id=0) and sub-windows (window_id>0) both proceed.
    let window_id = self.window_id.ok_or_else(|| {
      error::ExternalError::NotSupported(error::NotSupportedError::new())
    })?;
    // Tauri `ignore=true` (pass events through to windows below) ↔ OHOS `touchable=false`
    // (window does not consume touch/mouse events). The negation lives in this tao layer;
    // the facade client passes `touchable` through verbatim. See design D4 mapping table.
    if let Some(ref client) = self.window_client {
      let client = client.clone();
      self.runtime.spawn(async move {
        if let Err(e) = client.set_window_touchable(window_id, !ignore).await {
          warn!(
            "set_ignore_cursor_events: set_window_touchable failed for window {}: {:?}",
            window_id, e
          );
        }
      });
    } else {
      // WindowClient not initialized (e.g. during early init) — surface NotSupported,
      // matching the old TSFN-uninitialized error path.
      warn!(
        "set_ignore_cursor_events: WindowClient not initialized for window {}",
        window_id
      );
      return Err(error::ExternalError::NotSupported(
        error::NotSupportedError::new(),
      ));
    }
    Ok(())
  }

  pub fn set_cursor_visible(&self, visible: bool) {
    // TODO(issue 6): dispatched but not yet device-tested — verify scope (global vs
    //   window-level): pointer.setPointerVisible is a global cursor toggle, while
    //   tao's semantics are window-level; under multiple windows it would also
    //   affect other windows.
    //   See doc/OHOS-window-residual-issues.md (issue 6).
    // Global cursor visibility (pointer.setPointerVisible), dispatched via the
    // window bridge facade fire-and-forget (ArkTS side delegates to
    // WindowManager.setPointerVisible).
    // Restores the dispatch that the bridge facade migration dropped to a
    // no-op — the ArkTS implementation survived, only the Rust call was lost.
    let client = match &self.window_client {
      Some(c) => c.clone(),
      None => return,
    };
    self.runtime.spawn(async move {
      if let Err(e) = client.set_cursor_visible(visible).await {
        log::warn!("[tao-ohos] set_cursor_visible failed to dispatch: {:?}", e);
      }
    });
  }
  pub fn drag_window(&self) -> Result<(), error::ExternalError> {
    // OHOS startMoving (API14+) must be called in onTouch(TouchType.Down) —
    // cannot be triggered programmatically from Rust. Float sub-windows drag
    // via FloatPage title bar onTouch→startMoving; the main UIAbility window
    // has no such path, so this is a no-op there. Returns Ok (no error) since
    // drag is handled at the UI layer (FloatPage), not via this API.
    log::debug!(
      "[tao-ohos] drag_window: no-op (startMoving must be called from onTouch in FloatPage; window_id={:?})",
      self.window_id
    );
    Ok(())
  }

  pub fn drag_resize_window(
    &self,
    _direction: ResizeDirection,
  ) -> Result<(), error::ExternalError> {
    // OHOS enableDrag (API20+) allows/disables edge drag-resize, but cannot
    // programmatically trigger a specific direction resize. System handles
    // edge drag natively. This returns Ok (no error).
    // G10: log the success path too so callers can tell "edge-drag enabled"
    // apart from an actual directional resize — the _direction is ignored and
    // no directional resize is started. Mirrors the drag_window no-op log above.
    if let Some(window_id) = self.window_id {
      let client = match &self.window_client {
        Some(c) => c.clone(),
        None => return Ok(()),
      };
      self.runtime.spawn(async move {
        if let Err(e) = client.set_window_draggable(window_id, true).await {
          log::warn!(
            "[tao-ohos] set_window_draggable(true) failed for window {}: {:?}",
            window_id,
            e
          );
        } else {
          log::debug!(
            "[tao-ohos] drag_resize_window: no directional resize API (_direction ignored); enableDrag(true) set for window {}",
            window_id
          );
        }
      });
    }
    Ok(())
  }

  pub fn set_background_color(&self, color: Option<crate::window::RGBA>) {
    // Respect transparent flag: silently ignore background_color when transparent=true,
    // consistent with creation-time behavior and P3 spec.
    if self.transparent {
      log::debug!("[tao-ohos] set_background_color ignored: window is transparent");
      return;
    }
    let color_u32 = rgba_to_ohos_color(false, color).unwrap_or(0xFFFFFFFF);
    if let Some(window_id) = self.window_id {
      let client = match &self.window_client {
        Some(c) => c.clone(),
        None => return,
      };
      self.runtime.spawn(async move {
        if let Err(e) = client.set_window_background_color(window_id, color_u32).await {
          log::warn!("[tao-ohos] set_window_background_color failed for window {}: {:?}", window_id, e);
        }
      });
    }
  }

  pub fn theme(&self) -> Theme {
    // Issue 5, 5.2 theme backfill: read the global override; on FOLLOW fall back to app.config().
    // app.config().color_mode is continuously refreshed by ConfigChanged
    // (onConfigurationUpdated), reflecting system truth — so under FOLLOW mode it
    // stays in sync with the system without manual backfill.
    use openharmony_ability::ColorMode;
    match APP_THEME_OVERRIDE.load(Ordering::Relaxed) {
      THEME_OVERRIDE_DARK => Theme::Dark,
      THEME_OVERRIDE_LIGHT => Theme::Light,
      _ => {
        // FOLLOW: read system truth.
        match self.app.config().color_mode {
          ColorMode::Dark => Theme::Dark,
          // Light or NoSet (no ConfigChanged received before startup) → Light.
          _ => Theme::Light,
        }
      }
    }
  }

  pub fn set_theme(&self, theme: Option<Theme>) {
    use openharmony_ability::ColorMode;
    // Write override: Some → explicit override; None → FOLLOW (follow system).
    APP_THEME_OVERRIDE.store(
      match theme {
        Some(Theme::Dark) => THEME_OVERRIDE_DARK,
        Some(Theme::Light) => THEME_OVERRIDE_LIGHT,
        None => THEME_OVERRIDE_FOLLOW,
      },
      Ordering::Relaxed,
    );
    let color_mode = match theme {
      Some(Theme::Dark) => ColorMode::Dark,
      Some(Theme::Light) => ColorMode::Light,
      None => ColorMode::NoSet,
    };
    // Migrate from OpenHarmonyApp::set_color_mode (removed) to
    // ColorModeExt::set_color_mode (MainThreadSync bridge call).
    // Bridge contract: Dark=0, Light=1, NoSet=2.
    let mode_i32 = match color_mode {
      ColorMode::Dark => 0,
      ColorMode::Light => 1,
      ColorMode::NoSet => 2,
    };
    let env_cell = openharmony_ability::get_main_thread_env();
    let env_ref = env_cell.borrow();
    if let Some(env) = env_ref.as_ref() {
      if let Err(e) = self.app.set_color_mode(env, mode_i32) {
        log::warn!("set_theme: failed to call set_color_mode: {:?}", e);
      }
    } else {
      log::warn!("set_theme: main thread Env not available");
    }
  }

  pub fn title(&self) -> String {
    String::new()
  }

  #[cfg(feature = "rwh_04")]
  pub fn raw_window_handle_rwh_04(&self) -> rwh_04::RawWindowHandle {
    unreachable!("rwh_04 is not supported on OpenHarmony");
  }

  #[cfg(feature = "rwh_05")]
  pub fn raw_window_handle_rwh_05(&self) -> rwh_05::RawWindowHandle {
    unreachable!("rwh_05 is not supported on OpenHarmony");
  }

  #[cfg(feature = "rwh_05")]
  pub fn raw_display_handle_rwh_05(&self) -> rwh_05::RawDisplayHandle {
    unreachable!("rwh_05 is not supported on OpenHarmony");
  }

  #[cfg(feature = "rwh_06")]
  // Allow the usage of HasRawWindowHandle inside this function
  #[allow(deprecated)]
  pub fn raw_window_handle_rwh_06(&self) -> Result<rwh_06::RawWindowHandle, rwh_06::HandleError> {
    if let Some(native_window) = self.app.native_window().as_ref() {
      if let Some(win) = native_window.raw_window_handle() {
        return Ok(win);
      }
      Err(rwh_06::HandleError::Unavailable)
    } else {
      Err(rwh_06::HandleError::Unavailable)
    }
  }

  #[cfg(feature = "rwh_06")]
  pub fn raw_display_handle_rwh_06(&self) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
    Ok(rwh_06::RawDisplayHandle::Ohos(
      rwh_06::OhosDisplayHandle::new(),
    ))
  }

  pub fn config(&self) -> Configuration {
    self.app.config()
  }

  pub fn content_rect(&self) -> Rect {
    self.app.content_rect()
  }

  pub fn window_id(&self) -> Option<i64> {
    self.window_id
  }

  /// Returns the `BridgeRuntime` for this window's `OpenHarmonyApp`.
  /// Used by wry's bridge-based webview backend to construct `WebviewClient::from_bridge`.
  pub(crate) fn bridge_runtime(
    &self,
  ) -> openharmony_ability::napi_ohos::Result<openharmony_ability::BridgeRuntime> {
    self.app.bridge()
  }

  pub fn current_monitor(&self) -> Option<monitor::MonitorHandle> {
    Some(monitor::MonitorHandle {
      inner: MonitorHandle::new(self.app.clone()),
    })
  }

  pub fn primary_monitor(&self) -> Option<monitor::MonitorHandle> {
    Some(monitor::MonitorHandle {
      inner: MonitorHandle::new(self.app.clone()),
    })
  }
}

#[derive(Default, Clone, Debug)]
pub struct OsError;

use std::fmt::{self, Display, Formatter};
impl Display for OsError {
  fn fmt(&self, fmt: &mut Formatter<'_>) -> Result<(), fmt::Error> {
    write!(fmt, "OpenHarmony OS Error")
  }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonitorHandle {
  app: OpenHarmonyApp,
}

impl MonitorHandle {
  pub(crate) fn new(app: OpenHarmonyApp) -> Self {
    Self { app }
  }

  pub fn name(&self) -> Option<String> {
    Some("OpenHarmony Device".to_owned())
  }

  pub fn size(&self) -> PhysicalSize<u32> {
    // Real physical display dimensions — NOT the window's content_rect (which is
    // the window's own content area and is smaller than the screen). Using
    // content_rect here made positioner `Center` compute to negative coords
    // (content/2 - outer/2 < 0) which OHOS clamps to (0,0), so windows snapped
    // to top-left instead of centering.
    // Prefer OHOS DisplayManager physical pixels; fall back to content_rect
    // when the query returns 0. See ohos-monitor-real-values.
    let w = self.app.display_width();
    let h = self.app.display_height();
    if w > 0 && h > 0 {
      PhysicalSize::new(w, h)
    } else {
      warn!("[tao ohos] DisplayManager size query returned 0; falling back to content_rect");
      let size = self.app.content_rect();
      PhysicalSize::new(size.width as _, size.height as _)
    }
  }

  pub fn position(&self) -> PhysicalPosition<i32> {
    (0, 0).into()
  }

  pub fn scale_factor(&self) -> f64 {
    self.app.scale() as f64
  }

  pub fn video_modes(&self) -> impl Iterator<Item = monitor::VideoMode> {
    let size = self.size().into();
    // refresh_rate from OHOS DisplayManager real value (see ohos-monitor-real-values).
    // bit_depth fixed at 32 (RGBA8888) — see ohos-monitor-degradation.
    std::iter::once(monitor::VideoMode {
      video_mode: VideoMode {
        size,
        bit_depth: 32,
        refresh_rate: self.app.refresh_rate() as u16,
        monitor: self.clone(),
      },
    })
  }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct VideoMode {
  size: (u32, u32),
  bit_depth: u16,
  refresh_rate: u16,
  monitor: MonitorHandle,
}

impl VideoMode {
  pub fn size(&self) -> PhysicalSize<u32> {
    self.size.into()
  }

  pub fn bit_depth(&self) -> u16 {
    self.bit_depth
  }

  pub fn refresh_rate(&self) -> u16 {
    self.refresh_rate
  }

  pub fn monitor(&self) -> monitor::MonitorHandle {
    monitor::MonitorHandle {
      inner: self.monitor.clone(),
    }
  }
}

impl Drop for Window {
  /// Deregister the decor-change callback. Dropping the handle's sender AND
  /// removing the callback closure (which holds the other sender) closes the
  /// watcher channel from both ends, so `run_decor_watch` exits instead of
  /// parking forever. Windows that never took the correctable set_inner_size
  /// path (Float sub-windows) have no watcher — the take() is a no-op.
  fn drop(&mut self) {
    if let Some(handle) = self
      .decor_watch
      .lock()
      .expect("decor_watch poisoned")
      .take()
    {
      self.app.remove_decor_change_callback(handle.cb_id);
    }
  }
}

pub fn keycode_to_scancode(_code: KeyCode) -> Option<u32> {
  None
}

pub fn keycode_from_scancode(_scancode: u32) -> KeyCode {
  KeyCode::Unidentified(NativeKeyCode::Unidentified)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn rgba_to_ohos_color_transparent_returns_transparent_black() {
    assert_eq!(rgba_to_ohos_color(true, None), Some(0x00000000));
    assert_eq!(rgba_to_ohos_color(true, Some((255, 0, 0, 255))), Some(0x00000000));
  }

  #[test]
  fn rgba_to_ohos_color_none_bg_returns_none() {
    assert_eq!(rgba_to_ohos_color(false, None), None);
  }

  #[test]
  fn rgba_to_ohos_color_packs_argb() {
    assert_eq!(rgba_to_ohos_color(false, Some((255, 128, 0, 200))), Some(0xC8FF8000));
  }

  #[test]
  fn rgba_to_ohos_color_opaque_white() {
    assert_eq!(rgba_to_ohos_color(false, Some((255, 255, 255, 255))), Some(0xFFFFFFFF));
  }

  #[test]
  fn rgba_to_ohos_color_zero_alpha() {
    assert_eq!(rgba_to_ohos_color(false, Some((0, 0, 0, 0))), Some(0x00000000));
  }
}

/// Direct unit tests for the input-event handlers. These handlers are pure
/// transforms (OHOS input data -> tao events via an injected callback cell);
/// the app autotest never triggers them because no user input occurs, so we
/// exercise them directly with synthetic events.
#[cfg(test)]
mod input_tests {
  use super::*;
  use openharmony_ability::TextInputEventData;
  use openharmony_ability::xcomponent::{
    EventSource, KeyCode as OhosKeyCode, KeyEventData, TouchEventData, TouchPointData,
  };
  use std::sync::Mutex;

  type LoopCell<T> = Arc<RefCell<Option<Box<dyn FnMut(event::Event<T>) + 'static>>>>;

  /// Runs `invoke` with a collector installed in a fresh event-loop cell and
  /// returns compact descriptors of every event the handler emitted.
  fn run_collected<T: 'static>(invoke: impl FnOnce(&LoopCell<T>)) -> Vec<String> {
    let out: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = out.clone();
    let cell: LoopCell<T> = Arc::new(RefCell::new(Some(Box::new(move |e: event::Event<T>| {
      let desc = match e {
        event::Event::WindowEvent { event: we, .. } => match we {
          event::WindowEvent::CursorMoved { position, .. } => {
            format!("CursorMoved({},{})", position.x, position.y)
          }
          event::WindowEvent::MouseInput { state, button, .. } => {
            format!("MouseInput({:?},{:?})", state, button)
          }
          event::WindowEvent::CursorEntered { .. } => "CursorEntered".to_string(),
          event::WindowEvent::CursorLeft { .. } => "CursorLeft".to_string(),
          event::WindowEvent::MouseWheel { delta, modifiers, .. } => format!(
            "MouseWheel({:?},ctrl={})",
            delta,
            modifiers.contains(ModifiersState::CONTROL)
          ),
          event::WindowEvent::Touch(t) => format!(
            "Touch({:?},{},{},id={})",
            t.phase, t.location.x, t.location.y, t.id
          ),
          event::WindowEvent::KeyboardInput { event: ke, .. } => format!(
            "Key({:?},{:?},loc={:?},repeat={})",
            ke.state, ke.logical_key, ke.location, ke.repeat
          ),
          event::WindowEvent::ReceivedImeText(s) => format!("ImeText({s})"),
          _ => "Other".to_string(),
        },
        _ => "NonWindow".to_string(),
      };
      sink.lock().unwrap().push(desc);
    }))));
    invoke(&cell);
    let x = out.lock().unwrap().clone();
    x
  }

  fn mouse(action: MouseAction, button: OhosMouseButton) -> MouseEventData {
    MouseEventData { x: 10.5, y: 20.25, action, button, ..Default::default() }
  }

  // ─── handle_mouse_event ──────────────────────────────────────────────

  #[test]
  fn mouse_move_emits_cursor_moved() {
    let evs = run_collected(|cell| {
      EventLoop::<()>::handle_mouse_event(
        cell,
        &mouse(MouseAction::Move, OhosMouseButton::NoneButton),
      );
    });
    assert_eq!(evs, vec!["CursorMoved(10.5,20.25)".to_string()]);
  }

  #[test]
  fn mouse_press_release_left() {
    let evs = run_collected(|cell| {
      EventLoop::<()>::handle_mouse_event(
        cell,
        &mouse(MouseAction::Press, OhosMouseButton::LeftButton),
      );
      EventLoop::<()>::handle_mouse_event(
        cell,
        &mouse(MouseAction::Release, OhosMouseButton::LeftButton),
      );
    });
    assert_eq!(
      evs,
      vec![
        "MouseInput(Pressed,Left)".to_string(),
        "MouseInput(Released,Left)".to_string(),
      ]
    );
  }

  #[test]
  fn mouse_press_back_button_maps_to_other4() {
    let evs = run_collected(|cell| {
      EventLoop::<()>::handle_mouse_event(
        cell,
        &mouse(MouseAction::Press, OhosMouseButton::BackButton),
      );
      EventLoop::<()>::handle_mouse_event(
        cell,
        &mouse(MouseAction::Release, OhosMouseButton::ForwardButton),
      );
    });
    assert_eq!(
      evs,
      vec![
        "MouseInput(Pressed,Other(4))".to_string(),
        "MouseInput(Released,Other(5))".to_string(),
      ]
    );
  }

  #[test]
  fn mouse_press_none_button_emits_nothing() {
    let evs = run_collected(|cell| {
      EventLoop::<()>::handle_mouse_event(
        cell,
        &mouse(MouseAction::Press, OhosMouseButton::NoneButton),
      );
    });
    assert!(evs.is_empty());
  }

  #[test]
  fn mouse_hover_enter_leave() {
    let evs = run_collected(|cell| {
      EventLoop::<()>::handle_mouse_event(
        cell,
        &mouse(MouseAction::HoverEnter, OhosMouseButton::NoneButton),
      );
      EventLoop::<()>::handle_mouse_event(
        cell,
        &mouse(MouseAction::HoverLeave, OhosMouseButton::NoneButton),
      );
    });
    assert_eq!(
      evs,
      vec!["CursorEntered".to_string(), "CursorLeft".to_string()]
    );
  }

  #[test]
  fn mouse_none_action_emits_nothing() {
    let evs = run_collected(|cell| {
      EventLoop::<()>::handle_mouse_event(
        cell,
        &mouse(MouseAction::None, OhosMouseButton::NoneButton),
      );
    });
    assert!(evs.is_empty());
  }

  // ─── handle_axis_event ───────────────────────────────────────────────

  #[test]
  fn axis_mouse_wheel_uses_line_delta() {
    let evs = run_collected(|cell| {
      let d = AxisEventData {
        delta_x: 0.0,
        delta_y: 3.0,
        pinch_scale: 0.0,
        source_type: InputSourceType::Mouse,
        ..Default::default()
      };
      EventLoop::<()>::handle_axis_event(cell, &d);
    });
    assert_eq!(
      evs,
      vec!["MouseWheel(LineDelta(0.0, 3.0),ctrl=false)".to_string()]
    );
  }

  #[test]
  fn axis_touchpad_uses_pixel_delta() {
    let evs = run_collected(|cell| {
      let d = AxisEventData {
        delta_x: 10.0,
        delta_y: 20.0,
        pinch_scale: 0.0,
        source_type: InputSourceType::Touchpad,
        ..Default::default()
      };
      EventLoop::<()>::handle_axis_event(cell, &d);
    });
    assert_eq!(
      evs,
      vec!["MouseWheel(PixelDelta(PhysicalPosition { x: 10.0, y: 20.0 }),ctrl=false)".to_string()]
    );
  }

  #[test]
  fn axis_pinch_zoom_in_and_out_emit_ctrl_wheel() {
    let evs = run_collected(|cell| {
      let in_ = AxisEventData { pinch_scale: 1.5, ..Default::default() };
      let out_ = AxisEventData { pinch_scale: 0.5, ..Default::default() };
      EventLoop::<()>::handle_axis_event(cell, &in_);
      EventLoop::<()>::handle_axis_event(cell, &out_);
    });
    assert_eq!(
      evs,
      vec![
        "MouseWheel(LineDelta(0.0, 1.0),ctrl=true)".to_string(),
        "MouseWheel(LineDelta(0.0, -1.0),ctrl=true)".to_string(),
      ]
    );
  }

  #[test]
  fn axis_idle_event_emits_nothing() {
    let evs = run_collected(|cell| {
      EventLoop::<()>::handle_axis_event(cell, &AxisEventData::default());
    });
    assert!(evs.is_empty());
  }

  // ─── handle_input_event dispatch ─────────────────────────────────────

  #[test]
  fn input_event_routes_mouse() {
    let evs = run_collected(|cell| {
      EventLoop::<()>::handle_input_event(
        cell,
        &InputEvent::MouseEvent(mouse(MouseAction::Move, OhosMouseButton::NoneButton)),
      );
    });
    assert_eq!(evs, vec!["CursorMoved(10.5,20.25)".to_string()]);
  }

  #[test]
  fn input_event_routes_axis() {
    let evs = run_collected(|cell| {
      let d = AxisEventData {
        delta_y: 2.0,
        source_type: InputSourceType::Mouse,
        ..Default::default()
      };
      EventLoop::<()>::handle_input_event(cell, &InputEvent::AxisEvent(d));
    });
    assert_eq!(
      evs,
      vec!["MouseWheel(LineDelta(0.0, 2.0),ctrl=false)".to_string()]
    );
  }

  #[test]
  fn touch_down_emits_started_per_pointer() {
    let mut touch = TouchEventData { event_type: TouchEvent::Down, ..Default::default() };
    touch.touch_points = vec![
      TouchPointData {
        id: 7,
        x: 1.5,
        y: 2.5,
        force: 0.5,
        event_type: TouchEvent::Down,
        ..Default::default()
      },
      TouchPointData {
        id: 9,
        x: 3.5,
        y: 4.5,
        force: 0.25,
        event_type: TouchEvent::Down,
        ..Default::default()
      },
    ];
    let evs = run_collected(|cell| {
      EventLoop::<()>::handle_input_event(cell, &InputEvent::TouchEvent(touch));
    });
    assert_eq!(
      evs,
      vec![
        "Touch(Started,1.5,2.5,id=7)".to_string(),
        "Touch(Started,3.5,4.5,id=9)".to_string(),
      ]
    );
  }

  #[test]
  fn touch_move_up_cancel_phases() {
    for (ty, phase) in [
      (TouchEvent::Move, "Moved"),
      (TouchEvent::Up, "Ended"),
      (TouchEvent::Cancel, "Cancelled"),
    ] {
      let mut touch = TouchEventData { event_type: ty, ..Default::default() };
      touch.touch_points = vec![
        TouchPointData { id: 1, event_type: ty, ..Default::default() },
      ];
      let evs = run_collected(|cell| {
        EventLoop::<()>::handle_input_event(cell, &InputEvent::TouchEvent(touch.clone()));
      });
      assert_eq!(evs, vec![format!("Touch({phase},0,0,id=1)")], "phase {phase}");
    }
  }

  #[test]
  fn touch_unknown_event_type_emits_nothing() {
    let mut touch = TouchEventData { event_type: TouchEvent::Unknown, ..Default::default() };
    touch.touch_points = vec![
      TouchPointData { id: 1, event_type: TouchEvent::Unknown, ..Default::default() },
    ];
    let evs = run_collected(|cell| {
      EventLoop::<()>::handle_input_event(cell, &InputEvent::TouchEvent(touch));
    });
    assert!(evs.is_empty());
  }

  fn key(code: OhosKeyCode, action: Action) -> InputEvent {
    InputEvent::KeyEvent(KeyEventData {
      code,
      action,
      device_id: 3,
      source: EventSource::Keyboard,
      timestamp: 0,
    })
  }

  #[test]
  fn key_down_up_and_autorepeat() {
    PRESSED_KEYS.with(|k| k.borrow_mut().clear());
    let evs = run_collected(|cell| {
      EventLoop::<()>::handle_input_event(cell, &key(OhosKeyCode::A, Action::Down));
      EventLoop::<()>::handle_input_event(cell, &key(OhosKeyCode::A, Action::Down));
      EventLoop::<()>::handle_input_event(cell, &key(OhosKeyCode::A, Action::Up));
    });
    assert_eq!(evs.len(), 3);
    assert!(
      evs[0].starts_with("Key(Pressed,") && evs[0].ends_with(",repeat=false)"),
      "{}",
      evs[0]
    );
    assert!(evs[1].ends_with(",repeat=true)"), "{}", evs[1]);
    assert!(evs[2].starts_with("Key(Released,"), "{}", evs[2]);
    PRESSED_KEYS.with(|k| k.borrow_mut().clear());
  }

  #[test]
  fn key_location_for_modifier_pairs() {
    PRESSED_KEYS.with(|k| k.borrow_mut().clear());
    let evs = run_collected(|cell| {
      EventLoop::<()>::handle_input_event(cell, &key(OhosKeyCode::ShiftLeft, Action::Down));
      EventLoop::<()>::handle_input_event(cell, &key(OhosKeyCode::ShiftRight, Action::Down));
      EventLoop::<()>::handle_input_event(cell, &key(OhosKeyCode::Numpad5, Action::Down));
    });
    assert_eq!(evs.len(), 3);
    assert!(evs[0].contains("loc=Left"), "{}", evs[0]);
    assert!(evs[1].contains("loc=Right"), "{}", evs[1]);
    assert!(evs[2].contains("loc=Numpad"), "{}", evs[2]);
    PRESSED_KEYS.with(|k| k.borrow_mut().clear());
  }

  // ─── IME events ──────────────────────────────────────────────────────

  #[test]
  fn ime_text_input_emits_received_ime_text() {
    let evs = run_collected(|cell| {
      EventLoop::<()>::handle_input_event(
        cell,
        &InputEvent::ImeEvent(ImeEvent::TextInputEvent(TextInputEventData {
          text: "hello".to_string(),
        })),
      );
    });
    assert_eq!(evs, vec!["ImeText(hello)".to_string()]);
  }

  #[test]
  fn ime_backspace_and_enter_mock_press_release_pairs() {
    let evs = run_collected(|cell| {
      EventLoop::<()>::handle_input_event(
        cell,
        &InputEvent::ImeEvent(ImeEvent::BackspaceEvent(1)),
      );
      EventLoop::<()>::handle_input_event(
        cell,
        &InputEvent::ImeEvent(ImeEvent::EnterEvent(1)),
      );
    });
    assert_eq!(evs.len(), 4);
    assert!(evs[0].starts_with("Key(Pressed,Backspace"), "{}", evs[0]);
    assert!(evs[1].starts_with("Key(Released,Backspace"), "{}", evs[1]);
    assert!(evs[2].starts_with("Key(Pressed,Enter"), "{}", evs[2]);
    assert!(evs[3].starts_with("Key(Released,Enter"), "{}", evs[3]);
  }

  #[test]
  fn ime_status_hide_mocks_enter_show_is_ignored() {
    let evs = run_collected(|cell| {
      EventLoop::<()>::handle_input_event(
        cell,
        &InputEvent::ImeEvent(ImeEvent::ImeStatusEvent(KeyboardStatus::Hide)),
      );
      EventLoop::<()>::handle_input_event(
        cell,
        &InputEvent::ImeEvent(ImeEvent::ImeStatusEvent(KeyboardStatus::Show)),
      );
    });
    assert_eq!(evs.len(), 2);
    assert!(
      evs.iter().all(|e| e.starts_with("Key(") && e.contains("Enter")),
      "{evs:?}"
    );
  }
}

// --- S9 fmt batch: OsError Display (appended at file end, keeps existing line numbers) ---
#[cfg(test)]
mod fmt_tests {
  use super::OsError;

  #[test]
  fn os_error_display_writes_message() {
    assert_eq!(format!("{}", OsError), "OpenHarmony OS Error");
    assert_eq!(format!("{:?}", OsError), "OsError"); // Debug derive
  }
}
