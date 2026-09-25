use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CudaCapability {
    pub available: bool,
    pub device_count: u32,
    pub error: Option<String>,
}

pub fn cuda_capability() -> CudaCapability {
    match cuda_device_count() {
        Ok(device_count) if device_count > 0 => CudaCapability {
            available: true,
            device_count,
            error: None,
        },
        Ok(_) => CudaCapability {
            available: false,
            device_count: 0,
            error: Some("No CUDA devices found".into()),
        },
        Err(error) => CudaCapability {
            available: false,
            device_count: 0,
            error: Some(error),
        },
    }
}

#[cfg(all(feature = "cuda", windows))]
fn cuda_device_count() -> Result<u32, String> {
    use windows::core::s;
    use windows::Win32::Foundation::FreeLibrary;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};

    type CuInit = unsafe extern "system" fn(u32) -> i32;
    type CuDeviceGetCount = unsafe extern "system" fn(*mut i32) -> i32;

    unsafe {
        let library = LoadLibraryA(s!("nvcuda.dll"))
            .map_err(|error| format!("Failed to load CUDA driver: {error}"))?;
        let result = (|| {
            let init = GetProcAddress(library, s!("cuInit"))
                .ok_or_else(|| "CUDA driver is missing cuInit".to_string())?;
            let get_count = GetProcAddress(library, s!("cuDeviceGetCount"))
                .ok_or_else(|| "CUDA driver is missing cuDeviceGetCount".to_string())?;
            let init: CuInit = std::mem::transmute(init);
            let get_count: CuDeviceGetCount = std::mem::transmute(get_count);

            let status = init(0);
            if status != 0 {
                return Err(format!(
                    "CUDA driver initialization failed (error code {status})"
                ));
            }
            let mut count = 0;
            let status = get_count(&mut count);
            if status != 0 {
                return Err(format!(
                    "Failed to enumerate CUDA devices (error code {status})"
                ));
            }
            Ok(count.max(0) as u32)
        })();
        let _ = FreeLibrary(library);
        result
    }
}

/// Linux：`libcuda.so.1` 由 NVIDIA 驱动提供，等价于 Windows 上的 `nvcuda.dll`。
#[cfg(all(feature = "cuda", target_os = "linux"))]
fn cuda_device_count() -> Result<u32, String> {
    use std::ffi::{c_int, c_void};

    type CuInit = unsafe extern "C" fn(u32) -> c_int;
    type CuDeviceGetCount = unsafe extern "C" fn(*mut c_int) -> c_int;

    let library =
        unsafe { libc::dlopen(c"libcuda.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if library.is_null() {
        let message = unsafe {
            let error = libc::dlerror();
            if error.is_null() {
                "the CUDA driver library could not be loaded".to_string()
            } else {
                std::ffi::CStr::from_ptr(error)
                    .to_string_lossy()
                    .into_owned()
            }
        };
        return Err(format!("Failed to load the CUDA driver: {message}"));
    }

    let result = unsafe {
        let init = libc::dlsym(library, c"cuInit".as_ptr());
        let get_count = libc::dlsym(library, c"cuDeviceGetCount".as_ptr());
        if init.is_null() || get_count.is_null() {
            Err("CUDA driver is missing cuInit or cuDeviceGetCount".to_string())
        } else {
            let init: CuInit = std::mem::transmute(init);
            let get_count: CuDeviceGetCount = std::mem::transmute(get_count);
            let status = init(0);
            if status != 0 {
                Err(format!(
                    "CUDA driver initialization failed (error code {status})"
                ))
            } else {
                let mut count: c_int = 0;
                let status = get_count(&mut count);
                if status != 0 {
                    Err(format!(
                        "Failed to enumerate CUDA devices (error code {status})"
                    ))
                } else {
                    Ok(count.max(0) as u32)
                }
            }
        }
    };
    unsafe { libc::dlclose(library) };
    let _: *mut c_void = std::ptr::null_mut();
    result
}

#[cfg(not(all(feature = "cuda", any(windows, target_os = "linux"))))]
fn cuda_device_count() -> Result<u32, String> {
    Err("This build does not include the CUDA backend".into())
}
