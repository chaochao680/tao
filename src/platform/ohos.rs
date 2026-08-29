//! # OpenHarmony
//!
//! The OpenHarmony backend builds on (and exposes types from) the [`ohos-rs`](https://docs.rs/ohos-rs/) crate.
//!
//! Native OpenHarmony applications need some form of "glue" crate that is responsible
//! for defining the main entry point for your Rust application as well as tracking
//! various life-cycle events and synchronizing with the main thread.
//!
//! Winit uses the [openharmony-ability](https://docs.rs/openharmony-ability/) as a
//! glue crate.
//!

#![cfg(target_env = "ohos")]

use crate::event_loop::{EventLoop, EventLoopBuilder};
use crate::window::{Window, WindowAttributes, WindowBuilder};
use openharmony_ability::{Configuration, OpenHarmonyApp, Rect};

/// Additional methods on [`EventLoop`] that are specific to OpenHarmony.
pub trait EventLoopExtOpenHarmony {}

impl<T> EventLoopExtOpenHarmony for EventLoop<T> {}

/// Additional methods on [`Window`] that are specific to OpenHarmony.
pub trait WindowExtOpenHarmony {
  fn content_rect(&self) -> Rect;

  fn config(&self) -> Configuration;

  /// Returns the OS-level window ID, used to distinguish main (0) vs sub-windows.
  fn window_id(&self) -> Option<i64>;

  /// Returns the `BridgeRuntime` for constructing bridge-based facade clients
  /// (e.g. wry's `WebviewClient::from_bridge`).
  fn bridge_runtime(&self) -> openharmony_ability::BridgeRuntime;

  /// Backfills system window status into tao mirror bits (issue 5, 5.3).
  ///
  /// `status` is a raw OHOS `WindowStatusType` value (passed through from ArkTS
  /// `windowStatusChange`). Called by tauri-runtime-wry's OHOS drain block after
  /// routing to this window, updating the `visible`/`fullscreen` mirror bits to
  /// reflect system truth.
  fn apply_window_status(&self, status: i32);
}

impl WindowExtOpenHarmony for Window {
  fn content_rect(&self) -> Rect {
    self.window.content_rect()
  }

  fn config(&self) -> Configuration {
    self.window.config()
  }

  fn window_id(&self) -> Option<i64> {
    self.window.window_id()
  }

  fn bridge_runtime(&self) -> openharmony_ability::BridgeRuntime {
    self
      .window
      .bridge_runtime()
      .expect("BridgeRuntime not available — EventLoop not initialized")
  }

  fn apply_window_status(&self, status: i32) {
    self.window.apply_window_status(status);
  }
}

/// Additional methods on [`WindowAttributes`] that are specific to OpenHarmony.
pub trait WindowAttributesExtOpenHarmony {}

impl WindowAttributesExtOpenHarmony for WindowAttributes {}

/// Additional methods on [`WindowBuilder`] that are specific to OpenHarmony.
pub trait WindowBuilderExtOpenHarmony {
  /// Sets the window label, used as the OS-level window name for sub-windows.
  fn with_label(self, label: &str) -> Self;

  /// Sets the OHOS window kind.
  ///
  /// - `UIAbility`: Main window that reuses the existing UIAbility container. Only one can exist (singleton).
  /// - `Float`: Sub-window that creates a new OS-level floating window (TYPE_FLOAT).
  ///
  /// Default is `Float` when not specified. Use `UIAbility` for the main window.
  fn with_window_kind(self, kind: OHOSWindowKind) -> Self;

  /// Returns the current OHOS window kind, if set.
  fn ohos_window_kind(&self) -> Option<OHOSWindowKind>;
}

impl WindowBuilderExtOpenHarmony for WindowBuilder {
  fn with_label(mut self, label: &str) -> Self {
    self.platform_specific.label = Some(label.to_string());
    self
  }

  fn with_window_kind(mut self, kind: OHOSWindowKind) -> Self {
    self.platform_specific.window_kind = Some(kind);
    self
  }

  fn ohos_window_kind(&self) -> Option<OHOSWindowKind> {
    self.platform_specific.window_kind
  }
}

pub use crate::platform_impl::OHOSWindowKind;

pub trait EventLoopBuilderExtOpenHarmony {
  /// Associates the [`OpenHarmonyApp`] that was passed to `openharmony-ability::ability` with the event loop
  ///
  /// This must be called on OpenHarmony since the [`OpenHarmonyApp`] is not global state.
  fn with_openharmony_app(&mut self, app: OpenHarmonyApp) -> &mut Self;
}

impl<T> EventLoopBuilderExtOpenHarmony for EventLoopBuilder<T> {
  fn with_openharmony_app(&mut self, app: OpenHarmonyApp) -> &mut Self {
    self.platform_specific.openharmony_app = Some(app);
    self
  }
}

/// Re-export of the `openharmony-ability` API
///
/// Winit re-exports the `openharmony-ability` API for convenience so that most
/// applications can rely on the Winit crate to resolve the required version of
/// `openharmony-ability` and avoid any chance of a conflict between Winit and the
/// application crate.
///
///
/// For compatibility applications should then import the [`OpenHarmonyApp`] type for
/// their `init(app: OpenHarmonyApp)` function and use `openharmony-ability-derive` to
/// implement entry like:
/// ```rust
/// #[cfg(target_env = "ohos")]
/// use winit::platform::ohos::ability::OpenHarmonyApp;
/// use openharmony_ability_derive::ability;
///
/// #[ability]
/// fn init(app: OpenHarmonyApp) {
///     // ...
/// }
/// ```
pub mod ability {
  #[doc(no_inline)]
  pub use openharmony_ability::{OpenHarmonyApp, drain_pending_window_closes, drain_pending_window_status};

  #[doc(no_inline)]
  pub use openharmony_ability_derive::*;
}
