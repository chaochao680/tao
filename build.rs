// SPDX-License-Identifier: Apache-2.0
// Sets the `desktop` / `mobile` cfg on OpenHarmony targets based on
// `OHOS_DEVICE_TYPE`, mirroring `openharmony-ability/crates/ability/build.rs`.
//
// tao's build script cfgs do NOT propagate from the `tauri` crate (Cargo does
// not forward build-script cfg to dependencies), so tao must set its own per
// CLAUDE.md rule 3. Downstream crates (and future desktop-only behaviour in
// tao) can then gate on `cfg(desktop)` / `cfg(mobile)`.
fn main() {
  let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
  if target_env == "ohos" {
    println!("cargo:rerun-if-env-changed=OHOS_DEVICE_TYPE");
    let device_type = std::env::var("OHOS_DEVICE_TYPE").unwrap_or_else(|_| "mobile".to_string());
    let is_desktop = device_type == "desktop";
    println!("cargo:rustc-check-cfg=cfg(desktop)");
    println!("cargo:rustc-check-cfg=cfg(mobile)");
    if is_desktop {
      println!("cargo:rustc-cfg=desktop");
    } else {
      println!("cargo:rustc-cfg=mobile");
    }
  }
}
