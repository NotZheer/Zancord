//! Windows Desktop Duplication API implementation (Phase 6.1).
//!
//! Captures each monitor via `IDXGIOutputDuplication` — the classic, fully
//! Win32/COM path (no WinRT picker or window interop needed, unlike Windows
//! Graphics Capture). Frames arrive as packed BGRA at the DISPLAY resolution
//! (DDA has no scaling; the encoder re-initializes on dimension changes).
//!
//! Scope of this backend:
//! - Sources: displays only (window capture would need EnumWindows + the WGC
//!   interop; not implemented).
//! - No system audio (that would be WASAPI loopback; not implemented).
//!
//! NOTE: developed and cross-type-checked from a non-Windows host — run
//! `cargo check --target x86_64-pc-windows-msvc -p zancord-capture` (and a
//! real capture on a Windows machine) before relying on it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{bail, Context, Result};
use tracing::{debug, info, warn};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput1, IDXGIResource, DXGI_ERROR_ACCESS_LOST,
    DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

use crate::traits::{
    CaptureConfig, CaptureSource, CaptureSourceType, CapturedVideoFrame, PixelFormat,
    ScreenCapturer,
};

/// How long `AcquireNextFrame` blocks per poll; keeps stop latency bounded.
const ACQUIRE_TIMEOUT_MS: u32 = 100;

/// Desktop Duplication-backed capturer. The duplication object lives on a
/// dedicated capture thread (COM is initialized there); frames are forwarded
/// over a channel owned by this struct.
pub struct WindowsScreenCapturer {
    video_tx: Sender<CapturedVideoFrame>,
    video_rx: Receiver<CapturedVideoFrame>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    /// Thread-reported error (duplication lost, device gone, …).
    stream_error: Arc<Mutex<Option<String>>>,
    output_index: Option<u32>,
}

impl WindowsScreenCapturer {
    pub fn new() -> Self {
        let (video_tx, video_rx) = mpsc::channel();
        Self {
            video_tx,
            video_rx,
            stop: Arc::new(AtomicBool::new(false)),
            handle: None,
            stream_error: Arc::new(Mutex::new(None)),
            output_index: None,
        }
    }
}

impl Default for WindowsScreenCapturer {
    fn default() -> Self {
        Self::new()
    }
}

/// Enumerates DXGI outputs (monitors) as capture sources. Requires COM on the
/// calling thread; DXGI tolerates the process-wide init, but initialize
/// explicitly to be safe on any thread.
fn list_outputs() -> Result<Vec<CaptureSource>> {
    let mut sources = Vec::new();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let factory =
        unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }.context("CreateDXGIFactory1 failed")?;
    let mut adapter_index = 0u32;
    loop {
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Err(err) if err.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(err) => return Err(err).context("EnumAdapters1 failed"),
            Ok(adapter) => adapter,
        };
        let mut output_index = 0u32;
        loop {
            let _output = match unsafe { adapter.EnumOutputs(output_index) } {
                Err(err) if err.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(err) => return Err(err).context("EnumOutputs failed"),
                Ok(output) => output,
            };
            sources.push(CaptureSource {
                // Stable-ish id: adapter/output ordinal. Display topology
                // changes renumber, same as any index-based scheme.
                id: format!("output:{output_index}"),
                name: format!("Display {}", output_index + 1),
                source_type: CaptureSourceType::Display,
                thumbnail: None,
            });
            output_index += 1;
        }
        adapter_index += 1;
    }
    unsafe {
        CoUninitialize();
    }
    Ok(sources)
}

impl ScreenCapturer for WindowsScreenCapturer {
    fn available_sources(&self) -> Result<Vec<CaptureSource>> {
        list_outputs()
    }

    fn start_capture(&mut self, source: &CaptureSource, config: &CaptureConfig) -> Result<()> {
        if self.handle.is_some() {
            self.stop_capture()?;
        }
        let output_index = source
            .id
            .strip_prefix("output:")
            .context("malformed output source id")?
            .parse::<u32>()
            .context("malformed output index")?;

        // DDA cannot downscale: the desktop is captured at its native
        // resolution and the encoder re-initializes on dimension changes.
        if config.width != 0 && config.width < 3840 {
            warn!(
                requested = config.width,
                "Windows DDA captures at display resolution; the requested size is ignored"
            );
        }

        self.stop.store(false, Ordering::Relaxed);
        let stop = Arc::clone(&self.stop);
        let video_tx = self.video_tx.clone();
        let stream_error = Arc::clone(&self.stream_error);
        let handle = std::thread::Builder::new()
            .name("zancord-windows-capture".to_string())
            .spawn(move || {
                if let Err(err) = run_duplication_loop(output_index, &video_tx, &stop) {
                    warn!(error = %err, "desktop duplication loop ended with an error");
                    *stream_error.lock().expect("stream_error lock") = Some(err.to_string());
                }
            })
            .context("failed to spawn capture thread")?;

        self.handle = Some(handle);
        self.output_index = Some(output_index);
        info!(source = %source.name, "screen capture started (display resolution)");
        Ok(())
    }

    fn stop_capture(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let reported = self.stream_error.lock().expect("stream_error lock").take();
        if let Some(err) = reported {
            bail!("capture stream ended with an error: {err}");
        }
        info!("screen capture stopped");
        Ok(())
    }

    fn video_frame_rx(&self) -> &Receiver<CapturedVideoFrame> {
        &self.video_rx
    }

    fn audio_sample_rx(&self) -> Option<&Receiver<crate::traits::CapturedAudioFrame>> {
        // WASAPI loopback capture is not implemented for this backend.
        None
    }
}

/// Owns the duplication + D3D11 device for the lifetime of the capture thread.
fn run_duplication_loop(
    output_index: u32,
    video_tx: &Sender<CapturedVideoFrame>,
    stop: &AtomicBool,
) -> Result<()> {
    // COM must be initialized on the thread that touches COM objects.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let result = capture_until_stopped(output_index, video_tx, stop);
    unsafe {
        CoUninitialize();
    }
    result
}

fn capture_until_stopped(
    output_index: u32,
    video_tx: &Sender<CapturedVideoFrame>,
    stop: &AtomicBool,
) -> Result<()> {
    // Hardware device; BGRA support is required for `DXGI_FORMAT_B8G8R8A8_UNORM`.
    let mut device: Option<ID3D11Device> = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            None,
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
        .context("D3D11CreateDevice failed")?;
    }
    let device = device.context("no D3D11 device")?;
    let context: ID3D11DeviceContext =
        unsafe { device.GetImmediateContext() }.context("GetImmediateContext failed")?;

    let factory =
        unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }.context("CreateDXGIFactory1 failed")?;
    let adapter = unsafe { factory.EnumAdapters1(0) }.context("EnumAdapters1 failed")?;
    let output = unsafe { adapter.EnumOutputs(output_index) }
        .with_context(|| format!("output {output_index} not found (disconnected?)"))?;
    let output1: IDXGIOutput1 = output.cast().context("output is not IDXGIOutput1")?;
    let duplication = unsafe { output1.DuplicateOutput(&device) }
        .context("DuplicateOutput failed (desktop capture requires the app to be interactive)")?;

    let mut staging: Option<ID3D11Texture2D> = None;
    let mut staging_size = (0u32, 0u32);

    while !stop.load(Ordering::Relaxed) {
        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match unsafe {
            duplication.AcquireNextFrame(ACQUIRE_TIMEOUT_MS, &mut frame_info, &mut resource)
        } {
            Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => continue,
            Err(err) if err.code() == DXGI_ERROR_ACCESS_LOST => {
                // Display mode change / session switch: the duplication must
                // be recreated. Surface as an error; the app restarts capture.
                bail!("desktop duplication lost (display mode change?)");
            }
            Err(err) => return Err(err).context("AcquireNextFrame failed"),
            Ok(()) => {}
        }
        let Some(resource) = resource else {
            unsafe { duplication.ReleaseFrame() }.ok();
            continue;
        };
        let texture: ID3D11Texture2D = resource
            .cast()
            .context("captured resource is not a texture")?;

        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        let (width, height) = (desc.Width, desc.Height);
        if width == 0 || height == 0 {
            unsafe { duplication.ReleaseFrame() }.ok();
            continue;
        }

        // (Re)create the CPU-readable staging texture when the size changes.
        if staging.is_none() || staging_size != (width, height) {
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: desc.Format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut new_staging: Option<ID3D11Texture2D> = None;
            unsafe { device.CreateTexture2D(&staging_desc, None, Some(&mut new_staging)) }
                .context("CreateTexture2D (staging) failed")?;
            staging = new_staging;
            staging_size = (width, height);
            debug!(width, height, "staging texture (re)created");
        }
        let staging = staging.as_ref().context("staging texture missing")?;

        let staging_res: ID3D11Resource = staging.cast()?;
        let texture_res: ID3D11Resource = texture.cast()?;
        unsafe { context.CopyResource(&staging_res, &texture_res) };

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        if let Err(err) =
            unsafe { context.Map(&staging_res, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }
        {
            unsafe { duplication.ReleaseFrame() }.ok();
            return Err(err).context("Map failed");
        }
        // SAFETY: `mapped.pData` points at a readable staging buffer of at
        // least `RowPitch * height` bytes (we created it that size).
        let row_pitch = mapped.RowPitch as usize;
        let src = unsafe {
            std::slice::from_raw_parts(mapped.pData as *const u8, row_pitch * height as usize)
        };
        let mut data = Vec::with_capacity(width as usize * height as usize * 4);
        for row in 0..height as usize {
            data.extend_from_slice(&src[row * row_pitch..row * row_pitch + width as usize * 4]);
        }
        unsafe { context.Unmap(&staging_res, 0) };
        unsafe { duplication.ReleaseFrame() }.ok();

        if video_tx
            .send(CapturedVideoFrame {
                data,
                width,
                height,
                pixel_format: PixelFormat::Bgra,
                timestamp_us: 0,
            })
            .is_err()
        {
            bail!("video channel closed");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_without_start_is_ok() {
        let mut capturer = WindowsScreenCapturer::new();
        assert!(capturer.stop_capture().is_ok());
    }

    #[test]
    fn source_id_parses_output_index() {
        let source = CaptureSource {
            id: "output:3".into(),
            name: "Display 4".into(),
            source_type: CaptureSourceType::Display,
            thumbnail: None,
        };
        let index = source
            .id
            .strip_prefix("output:")
            .unwrap()
            .parse::<u32>()
            .unwrap();
        assert_eq!(index, 3);
    }
}
