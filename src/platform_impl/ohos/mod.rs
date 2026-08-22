use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::ffi::c_void;
use std::hash::Hash;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

use keycodes::{to_location, to_logical};
use openharmony_ability::window::{
  create_os_window, WindowCreateParams,
};
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

/// Local cursor position cache (f64 stored as u64 bits).
/// Updated in `handle_mouse_event` Move branch; read by `cursor_position()`.
/// Replaces the former `openharmony_ability::CURSOR_POSITION_X/Y` global atomics.
static CURSOR_X: AtomicU64 = AtomicU64::new(0);
static CURSOR_Y: AtomicU64 = AtomicU64::new(0);

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

/// Tracks currently pressed keys for repeat detection.
/// When a Down event arrives for a key already in this set, it's a repeat.
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
    _ => None,
  }
}

pub struct EventLoop<T: 'static> {
  pub(crate) openharmony_app: OpenHarmonyApp,
  bridge_executor: BridgeExecutor,
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
      bridge_executor: bridge_executor.clone(),
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
  fn handle_input_event(event_loop_cell: &Arc<RefCell<Option<Box<dyn FnMut(event::Event<T>) + 'static>>>>, event: &InputEvent) {
    #[allow(unreachable_patterns)]
    match event {
      InputEvent::TouchEvent(motion_event) => {
        let window_id = window::WindowId(WindowId);
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
              window_id: window::WindowId(WindowId),
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
              window_id: window::WindowId(WindowId),
              event: event::WindowEvent::ReceivedImeText(s.text.clone()),
            })
          }
        }
        ImeEvent::BackspaceEvent(_) => {
          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            // Mock keyboard input event
            let _ = [ElementState::Pressed, ElementState::Released].map(|state| {
              h(event::Event::WindowEvent {
                window_id: window::WindowId(WindowId),
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
                window_id: window::WindowId(WindowId),
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
                  window_id: window::WindowId(WindowId),
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
    let window_id = window::WindowId(WindowId);
    // Use device_id 0 for mouse, consistent across events.
    let device_id = event::DeviceId(DeviceId(0));

    match mouse_event.action {
      MouseAction::Move => {
        CURSOR_X.store((mouse_event.x as f64).to_bits(), Ordering::Relaxed);
        CURSOR_Y.store((mouse_event.y as f64).to_bits(), Ordering::Relaxed);
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
    let window_id = window::WindowId(WindowId);
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
        MainEvent::WindowResize(size) => {
          let size = PhysicalSize::new(size.width as _, size.height as _);
          let event = event::Event::WindowEvent {
            window_id: window::WindowId(WindowId),
            event: event::WindowEvent::Resized(size),
          };

          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event);
          }
        }
        MainEvent::WindowRedraw { .. } => {
          let event = event::Event::RedrawRequested(window::WindowId(WindowId));

          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event);
          }
        }
        MainEvent::ContentRectChange(content_rect) => {
          // Propagate as Resized so tauri's resize handler fires and calls
          // webview.set_bounds() with the new window dimensions.
          let size = PhysicalSize::new(content_rect.rect.width as _, content_rect.rect.height as _);
          let event = event::Event::WindowEvent {
            window_id: window::WindowId(WindowId),
            event: event::WindowEvent::Resized(size),
          };

          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event);
          }
        }
        MainEvent::GainedFocus => {
          HAS_FOCUS.store(true, Ordering::Relaxed);

          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event::Event::WindowEvent {
              window_id: window::WindowId(WindowId),
              event: event::WindowEvent::Focused(true),
            });
          }
        }
        MainEvent::LostFocus => {
          HAS_FOCUS.store(false, Ordering::Relaxed);

          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            h(event::Event::WindowEvent {
              window_id: window::WindowId(WindowId),
              event: event::WindowEvent::Focused(false),
            });
          }
        }
        MainEvent::ConfigChanged { .. } => {
          let size = app.content_rect();
          let scale = app.scale();
          let mut size = PhysicalSize::new(size.width as _, size.height as _);
          let event = event::Event::WindowEvent {
            window_id: window::WindowId(WindowId),
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
          //
          // Risk (ZST WindowId): WindowId is a ZST — every OHOS window hashes to the
          // same key (0), so tauri-runtime-wry's window_id_map.get(&ZST) returns the
          // *last-inserted* window, not necessarily the main window. If a Float
          // sub-window is still registered when WindowDestroy fires, these events
          // route to that Float window instead of the main window. This is acceptable
          // because WindowDestroy fires during UIAbility teardown (the app is exiting),
          // and tauri-runtime-wry's own close-drain is the authoritative per-window
          // channel. Tracked as a known issue in tauri-runtime-wry (see its TODO).
          if let Some(ref mut h) = *event_loop_cell.borrow_mut() {
            let e = event::Event::WindowEvent {
              window_id: window::WindowId(WindowId),
              event: event::WindowEvent::CloseRequested,
            };
            h(e);
            // Also dispatch Destroyed so tauri-runtime-wry can clean up the window.
            let destroyed = event::Event::WindowEvent {
              window_id: window::WindowId(WindowId),
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
            let mut drained = 0u32;
            while let Ok(event) = user_events_rx.borrow_mut().try_recv() {
              let event = event::Event::UserEvent(event);
              h(event);
              drained += 1;
            }
            if drained > 0 {
              log::info!("[DRAIN-DIAG] MainEvent::UserEvent drained {} events", drained);
            } else {
              log::info!("[DRAIN-DIAG] MainEvent::UserEvent fired but queue empty");
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
    let x = f64::from_bits(CURSOR_X.load(Ordering::Relaxed));
    let y = f64::from_bits(CURSOR_Y.load(Ordering::Relaxed));
    Ok(PhysicalPosition::new(x, y))
  }

  pub fn set_theme(&self, theme: Option<Theme>) {
    use openharmony_ability::ColorMode;
    let color_mode = match theme {
      Some(Theme::Dark) => ColorMode::Dark,
      Some(Theme::Light) | None => ColorMode::Light,
    };
    let color_mode = match theme {
      Some(_) => color_mode,
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

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct WindowId;

impl WindowId {
  pub const fn dummy() -> Self {
    WindowId
  }
}

impl From<WindowId> for u64 {
  fn from(_: WindowId) -> Self {
    0
  }
}

impl From<u64> for WindowId {
  fn from(_: u64) -> Self {
    Self
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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PlatformSpecificWindowBuilderAttributes {
  pub label: Option<String>,
  pub window_kind: Option<OHOSWindowKind>,
}

pub(crate) struct Window {
  app: OpenHarmonyApp,
  window_id: Option<i64>,
  /// Bridge facade for async window operations (None when bridge is not ready).
  window_client: Option<openharmony_ability_plugin_window::WindowClient>,
  /// Background runtime handle for spawning async bridge calls.
  runtime: BridgeExecutor,
  /// State cache for is_maximized() — updated synchronously in set_maximized().
  maximized: AtomicBool,
  /// State cache for is_minimized() — updated synchronously in set_minimized().
  minimized: AtomicBool,
  /// 0 = Light, 1 = Dark
  theme: AtomicU8,
  /// Phase 2: window decoration state (title bar visibility).
  /// AtomicBool supports runtime toggle from arbitrary threads.
  decorations: AtomicBool,
  /// Phase 3: whether window was created with transparent=true.
  /// Immutable after construction — set_background_color is a no-op when true.
  transparent: bool,
}

enum OHOSWindowType {
  TypeApp = 0,
  TypeSystemAlert = 1,
  TypeFloat = 8,
  TypeDialog = 16,
  TypeMain = 32,
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
    let is_main_window = match pl_attrs.window_kind {
      Some(OHOSWindowKind::UIAbility) => true,
      Some(OHOSWindowKind::Float) => false,
      None => !UIABILITY_CREATED.load(Ordering::SeqCst),
    };

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
    log::info!("[tao DBG] Window::new: window_id = {:?}", window_id);

    // Create the WindowClient bridge facade. If the bridge runtime is not yet
    // ready (e.g. during early init), window_client = None and all window
    // operations degrade to no-ops with a warn! log.
    let window_client = el.app.window().ok();
    let runtime = el.bridge_executor.clone();

    let win = Self {
      app: el.app.clone(),
      window_id,
      window_client,
      runtime,
      maximized: AtomicBool::new(false),
      minimized: AtomicBool::new(false),
      theme: AtomicU8::new(0),
      decorations: AtomicBool::new(window_attrs.decorations),
      transparent: window_attrs.transparent,
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

  pub fn request_redraw(&self) {}

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
    WindowId
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
    let window = self.app.window_rect();
    // inner_position = content area position on screen
    // = window position + content offset relative to window
    // content_rect.left/top is XComponent offset relative to its parent container
    // In OHOS: Screen -> Window -> Container -> XComponent
    Ok(PhysicalPosition::new(
      window.left + content.left,
      window.top + content.top,
    ))
  }

  pub fn inner_size(&self) -> PhysicalSize<u32> {
    // On OHOS desktop, win.resize(w, h) sets the OUTER size (including title bar).
    // Return window_rect (outer) so save→resize cycles are idempotent:
    // save inner_size (=outer) → restore via resize(outer) → outer unchanged.
    // The Web component uses .width("100%") (natural layout), so it does not
    // depend on inner_size for sizing — this change only affects window-state
    // save/restore and bounds rate calculations (unused for sizing with "100%").
    let rect = self.app.window_rect();
    PhysicalSize::new(rect.width as _, rect.height as _)
  }

  pub fn set_inner_size(&self, size: Size) {
    if let Some(window_id) = self.window_id {
      // OHOS win.resize(w, h) sets the OUTER size. inner_size() returns window_rect
      // (outer) so save→resize is idempotent on the PhysicalSize path (to_physical is
      // identity for PhysicalSize). For LogicalSize, convert via the real scale_factor
      // (a hardcoded 1.0 would halve the window on DPR≠1 displays). The ArkTS side
      // (WindowManager.resizeWindow) does NOT compensate — it calls win.resize(w, h)
      // directly, so the value passed here is the outer size.
      let physical = size.to_physical::<i32>(self.scale_factor());
      let client = match &self.window_client {
        Some(c) => c.clone(),
        None => return,
      };
      let w = physical.width as i64;
      let h = physical.height as i64;
      self.runtime.spawn(async move {
        if let Err(e) = client.resize_window(window_id, w, h).await {
          log::warn!("[tao-ohos] resize_window failed for window {}: {:?}", window_id, e);
        }
      });
    }
  }
  pub fn set_inner_size_constraints(&self, _: WindowSizeConstraints) {}

  pub fn outer_position(&self) -> Result<PhysicalPosition<i32>, error::NotSupportedError> {
    let rect = self.app.window_rect();
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
    let window = self.app.window_rect();
    // window_rect is set by ArkTS callback, may be (0,0,0,0) initially
    // fallback to content_rect if not yet initialized
    if window.width > 0 && window.height > 0 {
      PhysicalSize::new(window.width as _, window.height as _)
    } else {
      let content = self.app.content_rect();
      PhysicalSize::new(content.width as _, content.height as _)
    }
  }

  pub fn set_min_inner_size(&self, _: Option<Size>) {}

  pub fn set_max_inner_size(&self, _: Option<Size>) {}

  pub fn set_title(&self, _title: &str) {}

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
    log::warn!("`Window::is_always_on_top` is ignored on OpenHarmony");
    false
  }

  pub fn set_resizable(&self, _resizeable: bool) {
    warn!("`Window::set_resizable` is ignored on OpenHarmony")
  }

  pub fn set_minimizable(&self, _minimizable: bool) {
    warn!("`Window::set_minimizable` is ignored on OpenHarmony")
  }

  pub fn set_maximizable(&self, _maximizable: bool) {
    warn!("`Window::set_maximizable` is ignored on OpenHarmony")
  }

  pub fn set_closable(&self, _closable: bool) {
    warn!("`Window::set_closable` is ignored on OpenHarmony")
  }

  pub fn set_minimized(&self, minimized: bool) {
    // Update state cache synchronously before the async bridge call.
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
    // Update state cache synchronously before the async bridge call.
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
    // Sync the maximized cache: fullscreen implies maximized (entering fullscreen
    // is effectively maximize + immersive), exiting fullscreen calls recover()
    // which un-maximizes. Without this, is_maximized() returns stale state after
    // a fullscreen toggle, causing the next maximize/unmaximize to be a no-op.
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
    // Fullscreen::Exclusive/Borderless(MonitorHandle) state. There is no
    // reliable MonitorHandle to return, so report None. The actual fullscreen
    // state is driven imperatively via `set_fullscreen` above.
    None
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

  pub fn set_always_on_top(&self, _always_on_top: bool) {}
  pub fn set_ime_position(&self, _position: Position) {}

  pub fn is_decorated(&self) -> bool {
    self.decorations.load(Ordering::Acquire)
  }

  pub fn is_visible(&self) -> bool {
    log::warn!("`Window::is_visible` is ignored on OpenHarmony");
    false
  }

  pub fn is_resizable(&self) -> bool {
    warn!("`Window::is_resizable` is ignored on OpenHarmony");
    false
  }

  pub fn is_minimizable(&self) -> bool {
    warn!("`Window::is_minimizable` is ignored on OpenHarmony");
    false
  }

  pub fn is_maximizable(&self) -> bool {
    warn!("`Window::is_maximizable` is ignored on OpenHarmony");
    false
  }

  pub fn is_closable(&self) -> bool {
    warn!("`Window::is_closable` is ignored on OpenHarmony");
    false
  }

  pub fn set_window_icon(&self, _window_icon: Option<crate::icon::Icon>) {}

  pub fn set_cursor_icon(&self, _: window::CursorIcon) {}
  pub fn set_cursor_grab(&self, _: bool) -> Result<(), error::ExternalError> {
    Err(error::ExternalError::NotSupported(
      error::NotSupportedError::new(),
    ))
  }

  pub fn request_user_attention(&self, _request_type: Option<window::UserAttentionType>) {}

  pub fn set_cursor_position(&self, _: Position) -> Result<(), error::ExternalError> {
    Err(error::ExternalError::NotSupported(
      error::NotSupportedError::new(),
    ))
  }

  pub fn cursor_position(&self) -> Result<PhysicalPosition<f64>, error::ExternalError> {
    let x = f64::from_bits(CURSOR_X.load(Ordering::Relaxed));
    let y = f64::from_bits(CURSOR_Y.load(Ordering::Relaxed));
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

  pub fn set_cursor_visible(&self, _: bool) {}
  pub fn drag_window(&self) -> Result<(), error::ExternalError> {
    Err(error::ExternalError::NotSupported(
      error::NotSupportedError::new(),
    ))
  }

  pub fn drag_resize_window(
    &self,
    _direction: ResizeDirection,
  ) -> Result<(), error::ExternalError> {
    Err(error::ExternalError::NotSupported(
      error::NotSupportedError::new(),
    ))
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
    match self.theme.load(Ordering::Relaxed) {
      1 => Theme::Dark,
      _ => Theme::Light,
    }
  }

  pub fn set_theme(&self, theme: Option<Theme>) {
    use openharmony_ability::ColorMode;
    let color_mode = match theme {
      Some(Theme::Dark) => ColorMode::Dark,
      Some(Theme::Light) | None => ColorMode::Light,
    };
    // Store the resolved theme; None → Light (default)
    let stored = theme.unwrap_or(Theme::Light);
    self.theme.store(
      match stored {
        Theme::Dark => 1,
        Theme::Light => 0,
      },
      Ordering::Relaxed,
    );
    // If theme is None, follow system → NoSet
    let color_mode = match theme {
      Some(_) => color_mode,
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
pub fn keycode_to_scancode(_code: KeyCode) -> Option<u32> {
  None
}

pub fn keycode_from_scancode(_scancode: u32) -> KeyCode {
  KeyCode::Unidentified(NativeKeyCode::Unidentified)
}
