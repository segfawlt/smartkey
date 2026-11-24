pub mod guids;
pub mod state;
pub mod trace;

#[cfg(windows)]
pub mod register;
#[cfg(windows)]
mod tsf;

#[cfg(windows)]
pub use register::{
    DllCanUnloadNow, DllGetClassObject, DllMain, DllRegisterServer, DllUnregisterServer,
};
