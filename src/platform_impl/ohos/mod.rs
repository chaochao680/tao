use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::hash::Hash;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc;
use std::ptr::NonNull;
use std::ffi::c_void;

use keycodes::{to_location, to_logical};
use openharmony_ability::xcomponent::{Action, MouseButton as OhosMouseButton, TouchEvent};
use openharmony_ability::{MouseAction, MouseEventData, AxisEventData, InputSourceType};
use openharmony_ability::window::{
  create_os_window, WindowCreateParams, set_window_decorations, set_window_background_color,
  move_window_to, resize_window,
  maximize_window, minimize_window, restore_window, recover_window,
  show_window, hide_window, focus_window,
  is_window_maximized, is_window_minimized,
  set_fullscreen as ohos_set_fullscreen, set_window_touchable,
  set_window_decoration_flags, set_window_focusable,
  set_pointer_visible, set_pointer_style,
  start_ui_ability, next_window_id,
};

use openharmony_ability::{
  ime::KeyboardStatus, Configuration, Event as MainEvent, ImeEvent, InputEvent, OpenHarmonyApp,
  OpenHarmonyWaker, Rect,
};

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
  window_target: event_loop::EventLoopWindowTarget<T>,
  _cause: StartCause,
  user_events_sender: mpsc::Sender<T>,
  user_events_receiver: PeekableReceiver<T>,
  event_loop: RefCell<Option<Box<dyn FnMut(event::Event<T>)>>>,
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

    Self {
      openharmony_app: openharmony_app.clone(),
      window_target: event_loop::EventLoopWindowTarget {
        p: EventLoopWindowTarget {
          app: openharmony_app.clone(),
          _control_flow: Cell::new(ControlFlow::default()),
          exit: Cell::new(false),
          _marker: PhantomData,
        },
        _marker: PhantomData,
      },
      _cause: StartCause::Init,
      user_events_sender,
      user_events_receiver: PeekableReceiver::from_recv(user_events_receiver),
      event_loop: RefCell::new(None),
    }
  }

  pub(crate) fn window_target(&self) -> &event_loop::EventLoopWindowTarget<T> {
    &self.window_target
  }

  // TODO: For input event, we need some real examples to test it
  fn handle_input_event(&self, event: &InputEvent) {
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
            if let Some(ref mut h) = *self.event_loop.borrow_mut() {
              h(event);
            }
          }
        }
      }
      InputEvent::MouseEvent(mouse_event) => {
        self.handle_mouse_event(mouse_event);
      }
      InputEvent::AxisEvent(axis_event) => {
        self.handle_axis_event(axis_event);
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
            if let Some(ref mut h) = *self.event_loop.borrow_mut() {
              h(event);
            }
          }
        }
      }
      InputEvent::ImeEvent(data) => match data {
        ImeEvent::TextInputEvent(s) => {
          if let Some(ref mut h) = *self.event_loop.borrow_mut() {
            h(event::Event::WindowEvent {
              window_id: window::WindowId(WindowId),
              event: event::WindowEvent::ReceivedImeText(s.text.clone()),
            })
          }
        }
        ImeEvent::BackspaceEvent(_) => {
          if let Some(ref mut h) = *self.event_loop.borrow_mut() {
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
          if let Some(ref mut h) = *self.event_loop.borrow_mut() {
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
            if let Some(ref mut h) = *self.event_loop.borrow_mut() {
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
  fn handle_mouse_event(&self, mouse_event: &MouseEventData) {
    let window_id = window::WindowId(WindowId);
    // Use device_id 0 for mouse, consistent across events.
    let device_id = event::DeviceId(DeviceId(0));

    match mouse_event.action {
      MouseAction::Move => {
        let position = PhysicalPosition {
          x: mouse_event.x as f64,
          y: mouse_event.y as f64,
        };
        if let Some(ref mut h) = *self.event_loop.borrow_mut() {
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
          if let Some(ref mut h) = *self.event_loop.borrow_mut() {
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
          if let Some(ref mut h) = *self.event_loop.borrow_mut() {
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
        if let Some(ref mut h) = *self.event_loop.borrow_mut() {
          h(event::Event::WindowEvent {
            window_id,
            event: event::WindowEvent::CursorEntered { device_id },
          });
        }
      }
      MouseAction::HoverLeave => {
        if let Some(ref mut h) = *self.event_loop.borrow_mut() {
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
  fn handle_axis_event(&self, axis_event: &AxisEventData) {
    let window_id = window::WindowId(WindowId);
    let device_id = event::DeviceId(DeviceId(0));
    let is_touchpad = axis_event.source_type == InputSourceType::Touchpad;

    if let Some(ref mut h) = *self.event_loop.borrow_mut() {
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
    let target = &self.window_target;

    {
      let handle = unsafe {
        std::mem::transmute::<Box<dyn FnMut(event::Event<T>)>, Box<dyn FnMut(event::Event<T>)>>(
          Box::new(move |e| {
            event_handle(e, &target, &mut control_flow);
            // We need to dispatch it after every event callbacks.
            event_handle(event::Event::MainEventsCleared, &target, &mut control_flow);
          }),
        )
      };
      self.event_loop.replace(Some(handle));
    }

    self.openharmony_app.clone().run_loop(|event| {
      match event {
        MainEvent::SurfaceCreate { .. } => {
          if let Some(ref mut h) = *self.event_loop.borrow_mut() {
            h(event::Event::NewEvents(StartCause::Init));
            h(event::Event::Resumed);
          }
        }
        MainEvent::SurfaceDestroy { .. } => {
          if let Some(ref mut h) = *self.event_loop.borrow_mut() {
            h(event::Event::Suspended);
          }
        }
        MainEvent::WindowResize(size) => {
          let size = PhysicalSize::new(size.width as _, size.height as _);
          let event = event::Event::WindowEvent {
            window_id: window::WindowId(WindowId),
            event: event::WindowEvent::Resized(size),
          };

          if let Some(ref mut h) = *self.event_loop.borrow_mut() {
            h(event);
          }
        }
        MainEvent::WindowRedraw { .. } => {
          let event = event::Event::RedrawRequested(window::WindowId(WindowId));

          if let Some(ref mut h) = *self.event_loop.borrow_mut() {
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

          if let Some(ref mut h) = *self.event_loop.borrow_mut() {
            h(event);
          }
        }
        MainEvent::GainedFocus => {
          HAS_FOCUS.store(true, Ordering::Relaxed);

          if let Some(ref mut h) = *self.event_loop.borrow_mut() {
            h(event::Event::WindowEvent {
              window_id: window::WindowId(WindowId),
              event: event::WindowEvent::Focused(true),
            });
          }
        }
        MainEvent::LostFocus => {
          HAS_FOCUS.store(false, Ordering::Relaxed);

          if let Some(ref mut h) = *self.event_loop.borrow_mut() {
            h(event::Event::WindowEvent {
              window_id: window::WindowId(WindowId),
              event: event::WindowEvent::Focused(false),
            });
          }
        }
        MainEvent::ConfigChanged { .. } => {
          let size = self.openharmony_app.content_rect();
          let scale = self.openharmony_app.scale();
          let mut size = PhysicalSize::new(size.width as _, size.height as _);
          let event = event::Event::WindowEvent {
            window_id: window::WindowId(WindowId),
            event: event::WindowEvent::ScaleFactorChanged {
              new_inner_size: &mut size,
              scale_factor: scale as _,
            },
          };

          if let Some(ref mut h) = *self.event_loop.borrow_mut() {
            h(event);
          }
        }
        MainEvent::Start => {
          // XXX: how to forward this state to applications?
          warn!("TODO: forward onStart notification to application");
        }
        MainEvent::Resume { .. } => {
          if let Some(ref mut h) = *self.event_loop.borrow_mut() {
            h(event::Event::Resumed);
          }
        }
        MainEvent::SaveState { .. } => {
          // XXX: how to forward this state to applications?
          // XXX: also how do we expose state restoration to apps?
          warn!("TODO: forward saveState notification to application");
        }
        MainEvent::Pause => {
          debug!("App Paused - stopped running");
          // TODO: This is incorrect - will be solved in https://github.com/rust-windowing/winit/pull/3897
          // self.running = false;
        }
        MainEvent::WindowDestroy => {
          // OHOS window close is fully handled by tauri-runtime-wry's
          // drain_pending_window_closes, which uses the real OHOS window_id to
          // find the correct Tauri window and calls on_close_requested →
          // on_window_close (firing both CloseRequested and Destroyed with the
          // correct label). We must NOT fire any TaoWindowEvent here — tao's
          // WindowId is a ZST (same value for all windows), so the
          // Event::WindowEvent handler's window_id_map.get(&ZST) resolves to the
          // last-inserted window, which may be a different window than the one
          // being closed. This caused the main window's webview to be removed
          // from the manager when a secondary UIAbility window was closed.
        }
        MainEvent::Destroy => {
          if let Some(ref mut h) = *self.event_loop.borrow_mut() {
            h(event::Event::LoopDestroyed);
          }
        }
        MainEvent::Input(input_event) => {
          self.handle_input_event(&input_event);
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
          if let Some(ref mut h) = *self.event_loop.borrow_mut() {
            h(event::Event::Opened { urls });
          }
        }
        MainEvent::UserEvent { .. } => {
          if let Some(ref mut h) = *self.event_loop.borrow_mut() {
            if let Ok(event) = self.user_events_receiver.try_recv() {
              let event = event::Event::UserEvent(event);
              h(event);
            }
          }
        }
        unknown => {
          trace!("Unknown MainEvent {unknown:?} (ignored)");
        }
      };

      if self.window_target.p.exit.get() {
        if let Some(ref mut h) = *self.event_loop.borrow_mut() {
          h(event::Event::LoopDestroyed);
          self.openharmony_app.exit(0);
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
  pub fn monitor_from_point(&self, _x: f64, _y: f64) -> Option<MonitorHandle> {
    warn!("`Window::monitor_from_point` is ignored on OpenHarmony");
    return None;
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
    let x = f64::from_bits(openharmony_ability::CURSOR_POSITION_X.load(Ordering::Relaxed));
    let y = f64::from_bits(openharmony_ability::CURSOR_POSITION_Y.load(Ordering::Relaxed));
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
    if let Err(e) = self.app.set_color_mode(color_mode) {
      log::warn!("EventLoopWindowTarget::set_theme: failed to call setColorMode: {:?}", e);
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
/// All UIAbility windows are equal — no primary/secondary distinction. The first
/// UIAbility (windowId=0) reuses the existing main container; subsequent UIAbilities
/// (windowId>0) start a new EntryAbility instance via `context.startAbility`.
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
  /// 0 = Light, 1 = Dark
  theme: AtomicU8,
  /// Phase 2: window decoration state (title bar visibility).
  /// AtomicBool supports runtime toggle from arbitrary threads.
  decorations: AtomicBool,
  /// Phase 3: whether window was created with transparent=true.
  /// Immutable after construction. Transparent windows use 0x00000000 at
  /// creation; runtime set_background_color now also applies color (consistent
  /// with other platforms).
  transparent: bool,
  /// 窗口状态镜像。tao 侧维护，OHOS 事件回灌后更新（后续 MainEvent 扩展）。
  /// 默认 maximized/minimized=false，visible=true，fullscreen=false。
  maximized: AtomicBool,
  minimized: AtomicBool,
  visible: AtomicBool,
  fullscreen: AtomicBool,
  /// always_on_top 意图标志（OHOS 无直接 API，仅记录意图，见 set_always_on_top）。
  always_on_top: AtomicBool,
  /// 装饰按钮可用性位域。bit0 closable, bit1 maximizable, bit2 minimizable,
  /// bit3 resizable。默认 0b1111=15（全可用）。
  decoration_flags: AtomicU8,
}

/// 装饰按钮位域常量（与 openharmony-ability ArkTS 一致）。
const FLAG_CLOSABLE: u8 = 1;
const FLAG_MAXIMIZABLE: u8 = 2;
const FLAG_MINIMIZABLE: u8 = 4;
const FLAG_RESIZABLE: u8 = 8;
const FLAG_ALL_DECORATIONS: u8 = FLAG_CLOSABLE | FLAG_MAXIMIZABLE | FLAG_MINIMIZABLE | FLAG_RESIZABLE;

enum OHOSWindowType {
  TypeApp = 0,
  TypeSystemAlert = 1,
  TypeFloat = 8,
  TypeDialog = 16,
  TypeMain = 32
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
    bg.map(|(r, g, b, a)|
      ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32))
  }
}

impl Window {
  pub(crate) fn new<T: 'static>(
    el: &EventLoopWindowTarget<T>,
    window_attrs: window::WindowAttributes,
    pl_attrs: PlatformSpecificWindowBuilderAttributes,
  ) -> Result<Self, error::OsError> {
    // None defaults to UIAbility — all unspecified windows are UIAbility instances.
    let kind = pl_attrs.window_kind.unwrap_or(OHOSWindowKind::UIAbility);

    // The first window must be UIAbility — it reuses the EntryAbility's main
    // window container (windowId=0). A Float window as the first window is
    // invalid because there is no UIAbility container to attach to.
    if !UIABILITY_CREATED.load(Ordering::SeqCst) && kind != OHOSWindowKind::UIAbility {
      log::error!("First window must be UIAbility, got {:?} — cannot create Float before any UIAbility exists", kind);
      return Err(os_error!(OsError));
    }

    let is_ui_ability = matches!(kind, OHOSWindowKind::UIAbility);

    let window_type = if is_ui_ability {
      // UIAbility window does not need a window_type
      0
    } else {
      // Float sub-window uses TypeFloat
      OHOSWindowType::TypeFloat as i32
    };

    let window_id = match kind {
      OHOSWindowKind::UIAbility => {
        if !UIABILITY_CREATED.swap(true, Ordering::SeqCst) {
          // First UIAbility: reuse the existing main window container (id=0).
          Some(0)
        } else {
          // Subsequent UIAbility: pre-allocate id, start a new EntryAbility instance.
          let id = next_window_id();
          let label = pl_attrs.label.clone().unwrap_or_else(|| window_attrs.title.clone());
          let url = String::new();
          if let Err(e) = start_ui_ability(id, label, url, true, window_attrs.transparent) {
            log::error!("start_ui_ability failed: {:?}", e);
            return Err(os_error!(OsError));
          }
          Some(id)
        }
      }
      OHOSWindowKind::Float => {
        // Float window: create a new OS-level floating window via create_os_window.
        let label = pl_attrs.label.clone().unwrap_or_else(|| window_attrs.title.clone());
        let params = WindowCreateParams {
          name: label,
          window_type: window_type as i32,
          decorations: window_attrs.decorations,
          transparent: window_attrs.transparent,
          background_color: rgba_to_ohos_color(window_attrs.transparent, window_attrs.background_color),
          ..WindowCreateParams::default()
        };
        create_os_window(params).ok()
      }
    };

    let win = Self {
      app: el.app.clone(),
      window_id,
      theme: AtomicU8::new(0),
      decorations: AtomicBool::new(window_attrs.decorations),
      transparent: window_attrs.transparent,
      maximized: AtomicBool::new(false),
      minimized: AtomicBool::new(false),
      visible: AtomicBool::new(true),
      fullscreen: AtomicBool::new(false),
      always_on_top: AtomicBool::new(false),
      decoration_flags: AtomicU8::new(FLAG_ALL_DECORATIONS),
    };

    // Apply decorations immediately for the main window at creation time.
    // Without this, the main window retains its default OS decorations even if
    // the builder specified .decorations(false), because Window::set_decorations()
    // is only called later (if at all) by the user.
    if is_ui_ability && !window_attrs.decorations {
      let _ = set_window_decorations(0, false);
    }

    Ok(win)
  }

  pub fn request_redraw(&self) {}

  #[inline]
  pub fn monitor_from_point(&self, _x: f64, _y: f64) -> Option<monitor::MonitorHandle> {
    warn!("`Window::monitor_from_point` is ignored on OpenHarmony");
    return None;
  }

  pub fn id(&self) -> WindowId {
    WindowId
  }

  /// 返回 OHOS window_id（主窗口为 0，Float 子窗口 >0）。
  /// 用于调用 openharmony-ability 的窗口操作封装。
  fn ohos_win_id(&self) -> i64 {
    self.window_id.unwrap_or(0)
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
    Ok(PhysicalPosition::new(window.left + content.left, window.top + content.top))
  }

  pub fn inner_size(&self) -> PhysicalSize<u32> {
    let rect = self.app.content_rect();
    PhysicalSize::new(rect.width as _, rect.height as _)
  }

  pub fn set_inner_size(&self, size: Size) {
    let s = size.to_physical::<u32>(self.scale_factor());
    let _ = resize_window(self.ohos_win_id(), s.width as i64, s.height as i64);
  }
  pub fn set_inner_size_constraints(&self, _: WindowSizeConstraints) {}

  pub fn outer_position(&self) -> Result<PhysicalPosition<i32>, error::NotSupportedError> {
    let rect = self.app.window_rect();
    Ok(PhysicalPosition::new(rect.left, rect.top))
  }

  pub fn set_outer_position(&self, position: Position) {
    let p = position.to_physical::<i32>(self.scale_factor());
    let _ = move_window_to(self.ohos_win_id(), p.x as i64, p.y as i64);
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
    self.visible.store(visibility, Ordering::Release);
    let id = self.ohos_win_id();
    let _ = if visibility { show_window(id) } else { hide_window(id) };
  }

  pub fn set_focus(&self) {
    let _ = focus_window(self.ohos_win_id());
  }

  pub fn set_focusable(&self, focusable: bool) {
    let _ = set_window_focusable(self.ohos_win_id(), focusable);
  }

  pub fn is_focused(&self) -> bool {
    HAS_FOCUS.load(Ordering::Relaxed)
  }

  pub fn is_always_on_top(&self) -> bool {
    self.always_on_top.load(Ordering::Acquire)
  }

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

  /// 公共：更新一个装饰位并派发到 ArkTS（FloatPage LocalStorage）。
  fn set_decoration_flag(&self, flag: u8, on: bool) {
    let mut flags = self.decoration_flags.load(Ordering::Acquire);
    if on { flags |= flag; } else { flags &= !flag; }
    self.decoration_flags.store(flags, Ordering::Release);
    let _ = set_window_decoration_flags(self.ohos_win_id(), flags);
  }

  pub fn set_minimized(&self, minimized: bool) {
    let id = self.ohos_win_id();
    if minimized {
      if let Err(e) = minimize_window(id) { log::warn!("[tao-ohos] minimize_window failed for window {}: {}", id, e); }
    } else {
      if let Err(e) = restore_window(id) { log::warn!("[tao-ohos] restore_window failed for window {}: {}", id, e); }
    }
  }

  pub fn is_minimized(&self) -> bool {
    let id = self.ohos_win_id();
    is_window_minimized(id).unwrap_or_else(|e| {
      log::warn!("[tao-ohos] is_window_minimized failed for window {}: {}", id, e);
      false
    })
  }

  pub fn set_maximized(&self, maximized: bool) {
    let id = self.ohos_win_id();
    if maximized {
      if let Err(e) = maximize_window(id) { log::warn!("[tao-ohos] maximize_window failed for window {}: {}", id, e); }
    } else {
      // recover() switches MAXIMIZE/FULL_SCREEN → FLOATING (API7+, public)
      if let Err(e) = recover_window(id) { log::warn!("[tao-ohos] recover_window failed for window {}: {}", id, e); }
    }
  }

  pub fn is_maximized(&self) -> bool {
    let id = self.ohos_win_id();
    is_window_maximized(id).unwrap_or_else(|e| {
      log::warn!("[tao-ohos] is_window_maximized failed for window {}: {}", id, e);
      false
    })
  }

  pub fn set_fullscreen(&self, monitor: Option<Fullscreen>) {
    // OHOS 无独占全屏（Exclusive）概念，统一映射到 Borderless（沉浸式布局）。
    let on = monitor.is_some();
    self.fullscreen.store(on, Ordering::Release);
    let _ = ohos_set_fullscreen(self.ohos_win_id(), on);
  }

  pub fn fullscreen(&self) -> Option<Fullscreen> {
    if self.fullscreen.load(Ordering::Acquire) {
      Some(Fullscreen::Borderless(None))
    } else {
      None
    }
  }

  pub fn set_decorations(&self, decorations: bool) {
    self.decorations.store(decorations, Ordering::Release);
    if let Some(window_id) = self.window_id {
      let _ = set_window_decorations(window_id, decorations);
    }
  }
  pub fn set_always_on_bottom(&self, _always_on_bottom: bool) {}

  pub fn set_always_on_top(&self, always_on_top: bool) {
    // OHOS 无跨窗口 z-order 公开 API。Float 子窗口天然浮于主窗口之上；
    // 主 UIAbility 窗口的置顶由系统管理。此处仅记录意图，is_always_on_top 据此返回。
    // 后续若 OHOS 开放 setWindowType/z-level API，在此接入 openharmony-ability 封装。
    self.always_on_top.store(always_on_top, Ordering::Release);
    if always_on_top {
      log::warn!("`Window::set_always_on_top(true)` recorded but not enforced on OpenHarmony (no z-order API)");
    }
  }
  pub fn set_ime_position(&self, _position: Position) {}

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
    // 按 windowId 设置光标样式（pointer.setPointerStyleSync）。
    let style = ohos_pointer_style(icon);
    if let Err(e) = set_pointer_style(self.ohos_win_id(), style) {
      log::warn!("set_cursor_icon failed to dispatch: {:?}", e);
    }
  }
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
    let x = f64::from_bits(openharmony_ability::CURSOR_POSITION_X.load(Ordering::Relaxed));
    let y = f64::from_bits(openharmony_ability::CURSOR_POSITION_Y.load(Ordering::Relaxed));
    Ok(PhysicalPosition::new(x, y))
  }

  pub fn set_ignore_cursor_events(&self, ignore: bool) -> Result<(), error::ExternalError> {
    // tao 语义：ignore=true 表示忽略光标事件（点击穿透）。
    // OHOS setWindowTouchable：touchable=true 可触摸，false 穿透。故 touchable = !ignore。
    if let Err(e) = set_window_touchable(self.ohos_win_id(), !ignore) {
      log::warn!("set_ignore_cursor_events failed to dispatch: {:?}", e);
    }
    Ok(())
  }

  pub fn set_cursor_visible(&self, visible: bool) {
    // 全局光标显隐（@ohos.multimodalInput.pointer.setPointerVisible）。
    if let Err(e) = set_pointer_visible(visible) {
      log::warn!("set_cursor_visible failed to dispatch: {:?}", e);
    }
  }
  pub fn drag_window(&self) -> Result<(), error::ExternalError> {
    // OHOS SDK 无 startWindowMove 公开 API。Float 子窗口拖拽由 FloatPage
    // PanGesture 手柄处理（UI 层），不通过该编程式 API 暴露。
    Err(error::ExternalError::NotSupported(
      error::NotSupportedError::new(),
    ))
  }

  pub fn drag_resize_window(
    &self,
    _direction: ResizeDirection,
  ) -> Result<(), error::ExternalError> {
    // OHOS SDK 无 startWindowResize / Direction 枚举。同 A3，由 FloatPage 手柄处理。
    Err(error::ExternalError::NotSupported(
      error::NotSupportedError::new(),
    ))
  }

  pub fn set_background_color(&self, color: Option<crate::window::RGBA>) {
    // 与 macOS/Windows/Linux 一致：transparent 窗口也允许运行时改背景色。
    // 不再因 transparent 拦截（commit 7963af3a 误加的拦截与 commit message 矛盾，
    // 且其他平台均不拦截 transparent 窗口的 set_background_color）。
    log::info!(
      "[tao-ohos] set_background_color: transparent={}, color={:?}",
      self.transparent,
      color
    );
    let color_u32 = rgba_to_ohos_color(false, color).unwrap_or(0xFFFFFFFF);
    if let Some(window_id) = self.window_id {
      let _ = set_window_background_color(window_id, color_u32);
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
    self.theme.store(match stored {
      Theme::Dark => 1,
      Theme::Light => 0,
    }, Ordering::Relaxed);
    // If theme is None, follow system → NoSet
    let color_mode = match theme {
      Some(_) => color_mode,
      None => ColorMode::NoSet,
    };
    if let Err(e) = self.app.set_color_mode(color_mode) {
      log::warn!("set_theme: failed to call setColorMode: {:?}", e);
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
    let (width, height) = self.app.display_size();
    PhysicalSize::new(width, height)
  }

  pub fn position(&self) -> PhysicalPosition<i32> {
    (0, 0).into()
  }

  pub fn scale_factor(&self) -> f64 {
    self.app.scale() as f64
  }

  pub fn video_modes(&self) -> impl Iterator<Item = monitor::VideoMode> {
    let size = self.size().into();
    // FIXME this is not the real refresh rate
    // (it is guaranteed to support 32 bit color though)
    std::iter::once(monitor::VideoMode {
      video_mode: VideoMode {
        size,
        bit_depth: 32,
        refresh_rate: 60,
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

#[cfg(test)]
mod tests {
  use super::*;

  /// rgba_to_ohos_color：透明优先，否则打包 0xAARRGGBB。
  #[test]
  fn rgba_color_packing() {
    assert_eq!(rgba_to_ohos_color(true, None), Some(0x00000000));
    assert_eq!(rgba_to_ohos_color(true, Some((255, 0, 0, 255))), Some(0x00000000));
    // 不透明红：R=255,G=0,B=0,A=255 → 0xFFFF0000
    assert_eq!(rgba_to_ohos_color(false, Some((255, 0, 0, 255))), Some(0xFFFF0000));
    // 半透明：A=0x80
    assert_eq!(rgba_to_ohos_color(false, Some((0, 0, 0, 0x80))), Some(0x80000000));
    assert_eq!(rgba_to_ohos_color(false, None), None);
  }

  ///  tao CursorIcon → OHOS PointerStyle 数值映射。
  #[test]
  fn cursor_icon_mapping() {
    use crate::window::CursorIcon;
    assert_eq!(ohos_pointer_style(CursorIcon::Default), 0);
    assert_eq!(ohos_pointer_style(CursorIcon::Crosshair), 13);
    assert_eq!(ohos_pointer_style(CursorIcon::Hand), 19);
    assert_eq!(ohos_pointer_style(CursorIcon::Text), 26);
    assert_eq!(ohos_pointer_style(CursorIcon::Wait), 42);
    assert_eq!(ohos_pointer_style(CursorIcon::NotAllowed), 15);
    assert_eq!(ohos_pointer_style(CursorIcon::Copy), 14);
    assert_eq!(ohos_pointer_style(CursorIcon::Grab), 18);
    assert_eq!(ohos_pointer_style(CursorIcon::Grabbing), 17);
    assert_eq!(ohos_pointer_style(CursorIcon::ZoomIn), 27);
    assert_eq!(ohos_pointer_style(CursorIcon::EResize), 1);
    assert_eq!(ohos_pointer_style(CursorIcon::EwResize), 5);
    assert_eq!(ohos_pointer_style(CursorIcon::NwseResize), 12);
  }
}
