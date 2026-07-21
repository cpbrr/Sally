//! macOS system-audio capture via a Core Audio process tap (macOS 14.4+).
//!
//! This is the preferred no-virtual-device path on modern macOS: a
//! `CATapDescription` taps every process's output except our own (so the
//! translated-voice readout never re-enters the pipeline as an echo), the
//! tap is wrapped in a private aggregate device alongside the real default
//! output device, and audio arrives through a plain IOProc callback on that
//! aggregate device. Older macOS (13.0-14.3) doesn't have this API at all;
//! the caller falls back to ScreenCaptureKit, then a loopback device.
//!
//! Every step here is unsafe CoreAudio/CoreFoundation FFI with no existing
//! Rust wrapper to lean on (`objc2-core-audio` only ships raw bindings), and
//! this file has only ever been compile-verified in CI — no Mac hardware
//! was available to run it. Treat runtime failures here as expected until
//! it has been field-tested.

#![cfg(target_os = "macos")]

use super::{AudioSource, RawFrame};
use crate::error::{Result, SallyError};
use objc2::AnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioDevicePropertyDeviceUID,
    kAudioDevicePropertyNominalSampleRate, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioObjectUnknown,
    kAudioSubDeviceUIDKey, kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey,
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart,
    AudioDeviceStop, AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData,
    AudioObjectID, AudioObjectPropertyAddress, CATapDescription,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};
use objc2_core_foundation::{
    kCFBooleanFalse, kCFBooleanTrue, CFArray, CFDictionary, CFRetained, CFString, CFType,
    ConcreteType,
};
use objc2_foundation::{NSArray, NSNumber};
use std::ffi::{c_void, CStr};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

struct TapContext {
    session_start: Instant,
    tx: mpsc::Sender<RawFrame>,
    stop: Arc<AtomicBool>,
    sample_rate: u32,
}

/// Capture the whole system's audio (minus our own process) into `tx` until
/// `stop` is set. Fails cleanly when the process-tap API rejects the
/// request (permission denied, or the running OS predates 14.4 despite the
/// caller's version check).
pub fn spawn_tap_capture(
    session_start: Instant,
    tx: mpsc::Sender<RawFrame>,
    stop: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<()>> {
    super::spawn_with_ready_signal("Core Audio tap", move |ready_tx| {
        let setup = unsafe { setup_tap(session_start, tx, stop.clone()) };
        let (tap_id, aggregate_id, io_proc_id, ctx) = match setup {
            Ok(h) => {
                let _ = ready_tx.send(Ok(()));
                h
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };
        while !stop.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        unsafe { teardown(tap_id, aggregate_id, io_proc_id, ctx) };
    })
}

type SetupOutput = (AudioObjectID, AudioObjectID, AudioDeviceIOProcID, *mut TapContext);

unsafe fn setup_tap(
    session_start: Instant,
    tx: mpsc::Sender<RawFrame>,
    stop: Arc<AtomicBool>,
) -> Result<SetupOutput> {
    let output_device_id: AudioObjectID = get_property(
        kAudioObjectSystemObject as u32,
        kAudioHardwarePropertyDefaultOutputDevice,
    )
    .map_err(|e| SallyError::Audio(format!("default output device: {e}")))?;
    let output_uid = get_cfstring_property(output_device_id, kAudioDevicePropertyDeviceUID)
        .map_err(|e| SallyError::Audio(format!("default output device UID: {e}")))?;
    let sample_rate: f64 = get_property(output_device_id, kAudioDevicePropertyNominalSampleRate)
        .unwrap_or(48_000.0);

    let own_id = own_process_object_id()
        .map_err(|e| SallyError::Audio(format!("own process object: {e}")))?;
    let exclude_number = NSNumber::new_u32(own_id);
    let exclude = NSArray::from_slice(&[&*exclude_number]);
    let alloc = CATapDescription::alloc();
    let tap_desc = CATapDescription::initStereoGlobalTapButExcludeProcesses(alloc, &exclude);
    tap_desc.setPrivate(true);

    let mut tap_id: AudioObjectID = 0;
    let status = AudioHardwareCreateProcessTap(Some(&tap_desc), &mut tap_id);
    if status != 0 {
        return Err(SallyError::Audio(format!(
            "AudioHardwareCreateProcessTap failed: OSStatus {status} (Screen \
             Recording / audio capture permission may be required)"
        )));
    }
    let tap_uid = tap_desc.UUID().UUIDString().to_string();

    let aggregate_uid = format!("com.sally.app.tap.{}", std::process::id());
    let sub_device_dict = build_dict(&[(kAudioSubDeviceUIDKey, ct(output_uid.clone()))]);
    let sub_device_list = erase_array(CFArray::from_retained_objects(&[sub_device_dict]));
    let tap_dict = build_dict(&[
        (kAudioSubTapUIDKey, ct(cfstr(&tap_uid))),
        (
            kAudioSubTapDriftCompensationKey,
            ct(cf_bool(kCFBooleanTrue)),
        ),
    ]);
    let tap_list = erase_array(CFArray::from_retained_objects(&[tap_dict]));
    let aggregate_dict = build_dict(&[
        (kAudioAggregateDeviceNameKey, ct(cfstr("Sally Tap"))),
        (kAudioAggregateDeviceUIDKey, ct(cfstr(&aggregate_uid))),
        (kAudioAggregateDeviceMainSubDeviceKey, ct(output_uid)),
        (
            kAudioAggregateDeviceIsPrivateKey,
            ct(cf_bool(kCFBooleanTrue)),
        ),
        (
            kAudioAggregateDeviceIsStackedKey,
            ct(cf_bool(kCFBooleanFalse)),
        ),
        (
            kAudioAggregateDeviceTapAutoStartKey,
            ct(cf_bool(kCFBooleanTrue)),
        ),
        (kAudioAggregateDeviceSubDeviceListKey, ct(sub_device_list)),
        (kAudioAggregateDeviceTapListKey, ct(tap_list)),
    ]);

    let mut aggregate_id: AudioObjectID = 0;
    let status = AudioHardwareCreateAggregateDevice(
        AsRef::<CFDictionary>::as_ref(&*aggregate_dict),
        (&mut aggregate_id).into(),
    );
    if status != 0 {
        AudioHardwareDestroyProcessTap(tap_id);
        return Err(SallyError::Audio(format!(
            "AudioHardwareCreateAggregateDevice failed: OSStatus {status}"
        )));
    }

    let ctx = Box::into_raw(Box::new(TapContext {
        session_start,
        tx,
        stop,
        sample_rate: sample_rate.round() as u32,
    }));

    let mut io_proc_id: AudioDeviceIOProcID = None;
    let status = AudioDeviceCreateIOProcID(
        aggregate_id,
        Some(io_proc),
        ctx as *mut c_void,
        (&mut io_proc_id).into(),
    );
    if status != 0 {
        drop(Box::from_raw(ctx));
        AudioHardwareDestroyAggregateDevice(aggregate_id);
        AudioHardwareDestroyProcessTap(tap_id);
        return Err(SallyError::Audio(format!(
            "AudioDeviceCreateIOProcID failed: OSStatus {status}"
        )));
    }

    let status = AudioDeviceStart(aggregate_id, io_proc_id);
    if status != 0 {
        AudioDeviceDestroyIOProcID(aggregate_id, io_proc_id);
        drop(Box::from_raw(ctx));
        AudioHardwareDestroyAggregateDevice(aggregate_id);
        AudioHardwareDestroyProcessTap(tap_id);
        return Err(SallyError::Audio(format!(
            "AudioDeviceStart failed: OSStatus {status}"
        )));
    }

    Ok((tap_id, aggregate_id, io_proc_id, ctx))
}

unsafe fn teardown(
    tap_id: AudioObjectID,
    aggregate_id: AudioObjectID,
    io_proc_id: AudioDeviceIOProcID,
    ctx: *mut TapContext,
) {
    let status = AudioDeviceStop(aggregate_id, io_proc_id);
    if status != 0 {
        log::warn!("AudioDeviceStop failed during teardown: OSStatus {status}");
    }
    let status = AudioDeviceDestroyIOProcID(aggregate_id, io_proc_id);
    if status != 0 {
        log::warn!("AudioDeviceDestroyIOProcID failed during teardown: OSStatus {status}");
    }
    let status = AudioHardwareDestroyAggregateDevice(aggregate_id);
    if status != 0 {
        log::warn!("AudioHardwareDestroyAggregateDevice failed during teardown: OSStatus {status}");
    }
    let status = AudioHardwareDestroyProcessTap(tap_id);
    if status != 0 {
        log::warn!("AudioHardwareDestroyProcessTap failed during teardown: OSStatus {status}");
    }
    if !ctx.is_null() {
        drop(Box::from_raw(ctx));
    }
}

unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input_data: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output_data: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client_data: *mut c_void,
) -> i32 {
    if client_data.is_null() {
        return 0;
    }
    let ctx = &*(client_data as *const TapContext);
    if ctx.stop.load(Ordering::SeqCst) {
        return 0;
    }
    let list = input_data.as_ref();
    let n = list.mNumberBuffers as usize;
    if n == 0 {
        return 0;
    }
    // mBuffers is a bindgen flexible-array-member placeholder of length 1;
    // the real in-memory layout has mNumberBuffers contiguous AudioBuffers.
    let buffers_ptr = list.mBuffers.as_ptr();
    let mut per_buffer: Vec<&[f32]> = Vec::with_capacity(n);
    let mut channels_in_first = 1usize;
    for i in 0..n {
        let buf = &*buffers_ptr.add(i);
        if buf.mData.is_null() || buf.mNumberChannels == 0 {
            continue;
        }
        if i == 0 {
            channels_in_first = buf.mNumberChannels as usize;
        }
        let frames = buf.mDataByteSize as usize / 4 / buf.mNumberChannels as usize;
        let total = frames * buf.mNumberChannels as usize;
        per_buffer.push(std::slice::from_raw_parts(buf.mData as *const f32, total));
    }
    if per_buffer.is_empty() {
        return 0;
    }
    let samples: Vec<f32> = if n > 1 {
        // Non-interleaved: one buffer per channel, average across buffers.
        let frames = per_buffer.iter().map(|b| b.len()).min().unwrap_or(0);
        (0..frames)
            .map(|f| per_buffer.iter().map(|b| b[f]).sum::<f32>() / per_buffer.len() as f32)
            .collect()
    } else if channels_in_first > 1 {
        // Interleaved multi-channel: average across channels per frame.
        per_buffer[0]
            .chunks_exact(channels_in_first)
            .map(|frame| frame.iter().sum::<f32>() / channels_in_first as f32)
            .collect()
    } else {
        per_buffer[0].to_vec()
    };

    let _ = ctx.tx.try_send(RawFrame {
        source: AudioSource::System,
        t_ms: ctx.session_start.elapsed().as_millis() as u64,
        sample_rate: ctx.sample_rate,
        channels: 1,
        samples,
    });
    0
}

unsafe fn get_property<T: Copy>(object_id: AudioObjectID, selector: u32) -> Result<T> {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut size = std::mem::size_of::<T>() as u32;
    let mut value = std::mem::MaybeUninit::<T>::uninit();
    let out_ptr = NonNull::new(value.as_mut_ptr() as *mut c_void)
        .ok_or_else(|| SallyError::Audio("null property buffer".into()))?;
    let status = AudioObjectGetPropertyData(
        object_id,
        (&address).into(),
        0,
        std::ptr::null(),
        (&mut size).into(),
        out_ptr,
    );
    if status != 0 {
        return Err(SallyError::Audio(format!(
            "AudioObjectGetPropertyData(selector={selector:#x}) failed: OSStatus {status}"
        )));
    }
    Ok(value.assume_init())
}

unsafe fn get_cfstring_property(
    object_id: AudioObjectID,
    selector: u32,
) -> Result<CFRetained<CFString>> {
    let ptr: *mut CFString = get_property(object_id, selector)?;
    let nn = NonNull::new(ptr)
        .ok_or_else(|| SallyError::Audio("property query returned a null CFString".into()))?;
    Ok(CFRetained::from_raw(nn))
}

unsafe fn own_process_object_id() -> Result<AudioObjectID> {
    let pid: i32 = std::process::id() as i32;
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut size = std::mem::size_of::<AudioObjectID>() as u32;
    let mut out: AudioObjectID = 0;
    let out_ptr = NonNull::new(&mut out as *mut AudioObjectID as *mut c_void)
        .ok_or_else(|| SallyError::Audio("null property buffer".into()))?;
    let status = AudioObjectGetPropertyData(
        kAudioObjectSystemObject as u32,
        (&address).into(),
        std::mem::size_of::<i32>() as u32,
        (&pid as *const i32).cast(),
        (&mut size).into(),
        out_ptr,
    );
    if status != 0 || out == kAudioObjectUnknown {
        return Err(SallyError::Audio(format!(
            "could not resolve own process object (OSStatus {status})"
        )));
    }
    Ok(out)
}

fn cfstr(s: &str) -> CFRetained<CFString> {
    CFString::from_str(s)
}

/// The `kCFBooleanTrue`/`kCFBooleanFalse` statics are exposed as weak-linked
/// (nullable) externs by the bindings; they are always present at runtime on
/// any real macOS.
fn cf_bool(b: Option<&'static objc2_core_foundation::CFBoolean>) -> CFRetained<objc2_core_foundation::CFBoolean> {
    CFRetained::from(b.expect("kCFBoolean singleton always linked"))
}

fn ct<T>(v: CFRetained<T>) -> CFRetained<CFType>
where
    T: ConcreteType + 'static,
{
    v.into()
}

/// Discards a `CFArray<T>`'s specific element type, since only the erased
/// `CFArray` (default `Opaque` type params) implements `ConcreteType` and can
/// cross into `ct()`/the raw HAL calls.
fn erase_array<T: objc2_core_foundation::Type + ?Sized>(a: CFRetained<CFArray<T>>) -> CFRetained<CFArray> {
    CFRetained::from(AsRef::<CFArray>::as_ref(&*a))
}

/// Builds a `{CFString: CFType}` dictionary from `&CStr` key constants (the
/// `kAudioAggregateDevice*Key` family are plain C strings, not CFStrings)
/// paired with already-boxed CFType values.
fn build_dict(pairs: &[(&CStr, CFRetained<CFType>)]) -> CFRetained<CFDictionary<CFString, CFType>> {
    let keys: Vec<CFRetained<CFString>> = pairs
        .iter()
        .map(|(k, _)| cfstr(k.to_str().expect("ascii CoreAudio dictionary key")))
        .collect();
    let key_refs: Vec<&CFString> = keys.iter().map(|k| &**k).collect();
    let value_refs: Vec<&CFType> = pairs.iter().map(|(_, v)| &**v).collect();
    CFDictionary::from_slices(&key_refs, &value_refs)
}
