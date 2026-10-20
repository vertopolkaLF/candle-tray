//! COM objects stay on the monitor worker that created them.
use super::rules;
use windows::{
    Win32::System::{Com::*, Ole::*, Variant::*, Wmi::*},
    core::{BSTR, PCWSTR, w},
};

pub(super) struct Wmi {
    service: IWbemServices,
    _com: Com,
}
struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
struct Value(VARIANT);
impl Drop for Value {
    fn drop(&mut self) {
        unsafe {
            let _ = VariantClear(&mut self.0);
        }
    }
}
fn value(object: &IWbemClassObject, name: PCWSTR) -> windows::core::Result<Value> {
    let mut v = Value(VARIANT::default());
    unsafe {
        object.Get(name, 0, &mut v.0, None, None).ok()?;
    }
    Ok(v)
}
fn string(object: &IWbemClassObject, name: PCWSTR) -> Option<String> {
    let v = value(object, name).ok()?;
    unsafe {
        (v.0.Anonymous.Anonymous.vt == VT_BSTR)
            .then(|| String::from_utf16_lossy(&v.0.Anonymous.Anonymous.Anonymous.bstrVal))
    }
}
impl Wmi {
    pub fn connect() -> windows::core::Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
            let com = Com;
            // Security may already have been initialized by GPUI or another worker.
            let security = CoInitializeSecurity(
                None,
                -1,
                None,
                None,
                RPC_C_AUTHN_LEVEL_DEFAULT,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
                None,
            );
            if security.is_err() && security != windows::Win32::Foundation::RPC_E_TOO_LATE {
                return Err(security.into());
            }
            let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;
            let empty = BSTR::new();
            let service = locator.ConnectServer(
                &BSTR::from(r"ROOT\WMI"),
                &empty,
                &empty,
                &empty,
                WBEM_FLAG_CONNECT_USE_MAX_WAIT.0,
                &empty,
                None,
            )?;
            CoSetProxyBlanket(
                &service,
                10,
                0,
                None,
                RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )
            .ok()?;
            Ok(Self { service, _com: com })
        }
    }
    fn query(&self, class: &str) -> windows::core::Result<Vec<IWbemClassObject>> {
        unsafe {
            let enumerator = self.service.ExecQuery(
                &BSTR::from("WQL"),
                &BSTR::from(format!("SELECT * FROM {class}")),
                WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                None,
            )?;
            let mut result = Vec::new();
            loop {
                let mut objects = [None];
                let mut count = 0;
                let status = enumerator.Next(1000, &mut objects, &mut count);
                status.ok()?;
                if let Some(object) = objects[0].take() {
                    result.push(object);
                }
                if count == 0 {
                    break;
                }
            }
            Ok(result)
        }
    }
    pub fn name(&self, id: &str) -> Option<String> {
        for object in self.query("WmiMonitorID").ok()? {
            let Some(instance) = string(&object, w!("InstanceName")) else {
                continue;
            };
            if rules::identity(&instance).map(|(_, key)| key).as_deref() != Some(id) {
                continue;
            }
            let v = value(&object, w!("UserFriendlyName")).ok()?;
            unsafe {
                let raw = &v.0.Anonymous.Anonymous;
                let element_type = VARENUM(raw.vt.0 & !VT_ARRAY.0);
                if raw.vt.0 & VT_ARRAY.0 == 0
                    || !matches!(element_type, VT_UI2 | VT_I2 | VT_UI4 | VT_I4)
                {
                    continue;
                }
                let array = raw.Anonymous.parray;
                let lower = SafeArrayGetLBound(array, 1).ok()?;
                let upper = SafeArrayGetUBound(array, 1).ok()?;
                let mut chars = Vec::<u16>::new();
                for index in lower..=upper {
                    // CIM_UINT16 arrays may be marshalled as VT_I4. Allocate
                    // enough room for either representation before reading.
                    let mut c = 0u32;
                    SafeArrayGetElement(array, &index, &mut c as *mut _ as *mut _)
                        .ok()
                        .ok()?;
                    if c != 0 {
                        chars.push(u16::try_from(c).ok()?);
                    }
                }
                let name = String::from_utf16_lossy(&chars);
                return (!name.trim().is_empty()).then_some(name);
            }
        }
        None
    }
    pub fn brightness(&self, id: &str) -> Option<u8> {
        for object in self.query("WmiMonitorBrightness").ok()? {
            let Some(instance) = string(&object, w!("InstanceName")) else {
                continue;
            };
            if rules::identity(&instance).map(|(_, key)| key).as_deref() == Some(id) {
                let v = value(&object, w!("CurrentBrightness")).ok()?;
                return unsafe { VariantToUInt32(&v.0).ok().map(|n| n.min(100) as u8) };
            }
        }
        None
    }
    pub fn set(&self, id: &str, percent: u8) -> windows::core::Result<()> {
        unsafe {
            for object in self.query("WmiMonitorBrightnessMethods")? {
                let Some(instance) = string(&object, w!("InstanceName")) else {
                    continue;
                };
                if rules::identity(&instance).map(|(_, key)| key) != Some(id.to_owned()) {
                    continue;
                }
                let path =
                    string(&object, w!("__PATH")).ok_or_else(windows::core::Error::from_thread)?;
                // GetMethod requires a class definition, not an instance.
                let mut class = None;
                self.service
                    .GetObject(
                        &BSTR::from("WmiMonitorBrightnessMethods"),
                        WBEM_GENERIC_FLAG_TYPE(0),
                        None,
                        Some(&mut class),
                        None,
                    )
                    .ok()?;
                let mut signature = None;
                class
                    .ok_or_else(windows::core::Error::from_thread)?
                    .GetMethod(
                        w!("WmiSetBrightness"),
                        0,
                        &mut signature,
                        std::ptr::null_mut(),
                    )
                    .ok()?;
                let input = signature
                    .ok_or_else(windows::core::Error::from_thread)?
                    .SpawnInstance(0)?;
                let mut brightness = Value(VARIANT::default());
                (*brightness.0.Anonymous.Anonymous).vt = VT_UI1;
                (*brightness.0.Anonymous.Anonymous).Anonymous.bVal = percent.min(100);
                let mut timeout = Value(VARIANT::default());
                (*timeout.0.Anonymous.Anonymous).vt = VT_UI4;
                (*timeout.0.Anonymous.Anonymous).Anonymous.ulVal = 0;
                input.Put(w!("Brightness"), 0, &brightness.0, 0).ok()?;
                input.Put(w!("Timeout"), 0, &timeout.0, 0).ok()?;
                let mut output = None;
                self.service
                    .ExecMethod(
                        &BSTR::from(path),
                        &BSTR::from("WmiSetBrightness"),
                        WBEM_GENERIC_FLAG_TYPE(0),
                        None,
                        &input,
                        Some(&mut output),
                        None,
                    )
                    .ok()?;
                if let Some(output) = output {
                    let status = value(&output, w!("ReturnValue"))?;
                    if VariantToUInt32(&status.0)? != 0 {
                        return Err(windows::core::Error::from_thread());
                    }
                }
                return Ok(());
            }
            Err(windows::core::Error::from_thread())
        }
    }
}
