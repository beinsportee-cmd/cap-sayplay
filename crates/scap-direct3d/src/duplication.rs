//! Desktop Duplication (`IDXGIOutputDuplication`): a second way to capture a
//! display, for machines where Windows Graphics Capture is refused. Some
//! Windows installations answer `CreateForMonitor` or `CreateCaptureSession`
//! with E_ACCESSDENIED while duplicating the same monitor works fine.
//!
//! Frames come out as the same [`Frame`] the WGC [`Capturer`](crate::Capturer)
//! produces, in the requested pixel format, on the caller's device, so nothing
//! downstream knows which path made them. Desktop Duplication only ever
//! duplicates a whole output: there is no window capture here, and a hardware
//! cursor is not part of the image, so it is drawn in when the settings ask
//! for it. A software cursor (some drivers, or after some display changes) is
//! painted into the image by Windows and can't be left out;
//! [`DuplicationCapturer::cursor_in_image`] reports when that happens.

use std::{
    ffi::c_void,
    ptr::{null, null_mut},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{E_FAIL, HMODULE, LUID, POINT, RECT},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, ID3DBlob},
            Direct3D11::{
                D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BIND_UNORDERED_ACCESS,
                D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
                D3D11_MAPPED_SUBRESOURCE, D3D11_RESOURCE_MISC_GDI_COMPATIBLE, D3D11_SDK_VERSION,
                D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11CreateDevice,
                ID3D11ComputeShader, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
                ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11UnorderedAccessView,
            },
            Dxgi::{
                Common::{
                    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION_IDENTITY,
                    DXGI_MODE_ROTATION_UNSPECIFIED, DXGI_SAMPLE_DESC,
                },
                CreateDXGIFactory1, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
                IDXGIAdapter1, IDXGIDevice, IDXGIFactory1, IDXGIOutput1, IDXGIOutput5,
                IDXGIOutputDuplication, IDXGIResource, IDXGISurface1,
            },
            Gdi::{DeleteObject, HGDIOBJ, HMONITOR},
        },
        System::{
            LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
            Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
        },
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
            WindowsAndMessaging::{
                CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, GetCursorInfo, GetIconInfo,
                HCURSOR, HICON, ICONINFO,
            },
        },
    },
    core::{HRESULT, Interface, PCSTR, s, w},
};

use crate::{Frame, PixelFormat, Settings, StagingTexturePool, TrimPool};

/// How long one wait for a desktop update lasts; also how quickly `stop`
/// is noticed.
const ACQUIRE_TIMEOUT_MS: u32 = 100;
/// Reopening after the duplication is lost (a mode change, a full-screen
/// app, the secure desktop of a UAC prompt or the lock screen) backs off
/// between these.
const MIN_REOPEN_DELAY: Duration = Duration::from_millis(50);
const MAX_REOPEN_DELAY: Duration = Duration::from_millis(500);
/// A display whose output stays missing this long was unplugged.
const OUTPUT_GONE_AFTER: Duration = Duration::from_secs(3);

/// Copies the duplicated BGRA desktop into an RGBA texture. Sampling through
/// the BGRA view and storing through the RGBA one is the channel swap; the
/// desktop's alpha is not meaningful, so it is made opaque like WGC's.
const CONVERT_SHADER: &str = r"
Texture2D<float4> source : register(t0);
RWTexture2D<unorm float4> target : register(u0);

[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID)
{
    target[id.xy] = float4(source[id.xy].rgb, 1.0);
}
";

#[derive(Clone, Debug, thiserror::Error)]
pub enum NewDuplicationError {
    #[error("No graphics adapter drives this display")]
    OutputNotFound,
    #[error("Desktop Duplication can't record a rotated display")]
    RotatedOutput,
    #[error("CreateDevice: {0}")]
    CreateDevice(windows::core::Error),
    #[error("DuplicateOutput: {0}")]
    DuplicateOutput(windows::core::Error),
    #[error("The capture thread stopped during setup")]
    ThreadExited,
    #[error("Other: {0}")]
    Other(#[from] windows::core::Error),
}

/// How a duplicated frame reaches the caller's device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuplicationPath {
    /// The display's adapter is the caller's: frames are converted on the GPU.
    Gpu,
    /// Frames pass through system memory: the display is driven by another
    /// adapter (a hybrid laptop's integrated GPU), or the GPU conversion is
    /// unavailable.
    Cpu,
}

pub struct DuplicationCapturer {
    settings: Settings,
    path: DuplicationPath,
    start_tx: Option<mpsc::Sender<()>>,
    stop_flag: Arc<AtomicBool>,
    cursor_in_image: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl DuplicationCapturer {
    /// Opens the duplication before returning, so a display Desktop
    /// Duplication can't record fails here rather than after `start`. Frames
    /// are delivered from a thread of its own once `start` is called;
    /// `closed_callback` runs when the display goes away or the caller's
    /// device is removed, after which no more frames come.
    pub fn new(
        monitor: HMONITOR,
        settings: Settings,
        callback: impl FnMut(Frame) -> windows::core::Result<()> + Send + 'static,
        closed_callback: impl FnMut() -> windows::core::Result<()> + Send + 'static,
        d3d_device: Option<ID3D11Device>,
    ) -> Result<Self, NewDuplicationError> {
        let device = match d3d_device {
            Some(device) => device,
            None => {
                crate::create_d3d_device_with_warp_fallback()
                    .map_err(NewDuplicationError::CreateDevice)?
                    .0
            }
        };
        // HMONITOR is a pointer, so it travels to the thread as an integer.
        let monitor = monitor.0 as isize;
        let stop_flag = Arc::new(AtomicBool::new(false));
        let cursor_in_image = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (start_tx, start_rx) = mpsc::channel();

        let thread = std::thread::Builder::new()
            .name("scap-duplication".to_string())
            .spawn({
                let settings = settings.clone();
                let stop_flag = stop_flag.clone();
                let cursor_in_image = cursor_in_image.clone();
                move || {
                    run(
                        HMONITOR(monitor as *mut c_void),
                        settings,
                        device,
                        stop_flag,
                        cursor_in_image,
                        ready_tx,
                        start_rx,
                        callback,
                        closed_callback,
                    )
                }
            })
            .map_err(|error| {
                NewDuplicationError::Other(windows::core::Error::new(E_FAIL, error.to_string()))
            })?;

        match ready_rx.recv() {
            Ok(Ok(path)) => Ok(Self {
                settings,
                path,
                start_tx: Some(start_tx),
                stop_flag,
                cursor_in_image,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err(NewDuplicationError::ThreadExited)
            }
        }
    }

    pub fn path(&self) -> DuplicationPath {
        self.path
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Whether, while the cursor was meant to stay out of the frames,
    /// Windows painted it into the desktop image (a software cursor), so the
    /// delivered frames show it anyway. Only detected while the pointer moves
    /// over the display; once true it stays true.
    pub fn cursor_in_image(&self) -> bool {
        self.cursor_in_image.load(Ordering::Relaxed)
    }

    pub fn start(&mut self) -> windows::core::Result<()> {
        self.start_tx
            .as_ref()
            .and_then(|tx| tx.send(()).ok())
            .ok_or_else(|| windows::core::Error::new(E_FAIL, "Desktop Duplication has stopped"))
    }

    pub fn stop(&mut self) -> windows::core::Result<()> {
        self.stop_flag.store(true, Ordering::SeqCst);
        self.start_tx = None;
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("scap-direct3d: the Desktop Duplication thread panicked");
        }
        Ok(())
    }
}

impl Drop for DuplicationCapturer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    monitor: HMONITOR,
    settings: Settings,
    device: ID3D11Device,
    stop_flag: Arc<AtomicBool>,
    cursor_in_image: Arc<AtomicBool>,
    ready_tx: mpsc::SyncSender<Result<DuplicationPath, NewDuplicationError>>,
    start_rx: mpsc::Receiver<()>,
    mut callback: impl FnMut(Frame) -> windows::core::Result<()>,
    mut closed_callback: impl FnMut() -> windows::core::Result<()>,
) {
    // Output and cursor coordinates in physical pixels, whatever the process
    // declares; `DuplicateOutput1` also requires it.
    unsafe {
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let shared = match Shared::new(device, settings.pixel_format, cursor_in_image) {
        Ok(shared) => shared,
        Err(error) => {
            let _ = ready_tx.send(Err(error.into()));
            return;
        }
    };
    let session = match Session::open(monitor, &settings, &shared) {
        Ok(session) => {
            let _ = ready_tx.send(Ok(session.path()));
            session
        }
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    drop(ready_tx);

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            return;
        }
        match start_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(()) => break,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }

    let mut session = Some(session);
    // The first frame of every (re)opened duplication is delivered even
    // when Windows reports no new present, so the recording starts, and
    // resumes after a resolution change, with the current image.
    let mut deliver_next = true;
    let mut reopen_delay = MIN_REOPEN_DELAY;
    let mut output_missing_since: Option<Instant> = None;

    while !stop_flag.load(Ordering::Relaxed) {
        let Some(active) = session.as_mut() else {
            if shared.is_removed() {
                tracing::error!("scap-direct3d: capture device removed during Desktop Duplication");
                let _ = closed_callback();
                return;
            }
            std::thread::sleep(reopen_delay);
            match Session::open(monitor, &settings, &shared) {
                Ok(reopened) => {
                    tracing::info!(path = ?reopened.path(), "scap-direct3d: Desktop Duplication reopened");
                    session = Some(reopened);
                    deliver_next = true;
                    reopen_delay = MIN_REOPEN_DELAY;
                    output_missing_since = None;
                }
                Err(NewDuplicationError::OutputNotFound) => {
                    let since = *output_missing_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= OUTPUT_GONE_AFTER {
                        tracing::warn!("scap-direct3d: the duplicated display is gone");
                        let _ = closed_callback();
                        return;
                    }
                }
                Err(error) => {
                    tracing::debug!(%error, "scap-direct3d: Desktop Duplication not available yet");
                    output_missing_since = None;
                    reopen_delay = (reopen_delay * 2).min(MAX_REOPEN_DELAY);
                }
            }
            continue;
        };

        match active.next_frame(&shared, &settings, deliver_next) {
            Ok(Some(frame)) => {
                deliver_next = false;
                if let Err(error) = callback(frame) {
                    tracing::debug!(%error, "scap-direct3d: frame callback failed");
                }
            }
            Ok(None) => {}
            Err(error) => {
                if shared.is_removed() {
                    tracing::error!(%error, "scap-direct3d: capture device removed during Desktop Duplication");
                    let _ = closed_callback();
                    return;
                }
                tracing::info!(%error, "scap-direct3d: Desktop Duplication interrupted, reopening");
                session = None;
                reopen_delay = MIN_REOPEN_DELAY;
            }
        }
    }
}

/// A device plus its immediate context and multithread lock.
struct Device {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    multithread: Option<ID3D11Multithread>,
}

impl Device {
    fn new(device: ID3D11Device) -> windows::core::Result<Self> {
        let context = unsafe { device.GetImmediateContext() }?;
        let multithread = device.cast::<ID3D11Multithread>().ok();
        if let Some(multithread) = &multithread {
            unsafe {
                let _ = multithread.SetMultithreadProtected(true);
            }
        }
        Ok(Self {
            device,
            context,
            multithread,
        })
    }

    fn create_on(adapter: &IDXGIAdapter1) -> windows::core::Result<Self> {
        let mut device = None;
        unsafe {
            D3D11CreateDevice(
                adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )?;
        }
        Self::new(device.ok_or_else(windows::core::Error::from_win32)?)
    }

    /// Keeps a sequence of context calls together while other threads
    /// (the encoder) share the context.
    fn lock(&self) -> DeviceLock<'_> {
        if let Some(multithread) = &self.multithread {
            unsafe { multithread.Enter() };
        }
        DeviceLock(self.multithread.as_ref())
    }
}

struct DeviceLock<'a>(Option<&'a ID3D11Multithread>);

impl Drop for DeviceLock<'_> {
    fn drop(&mut self) {
        if let Some(multithread) = self.0 {
            unsafe { multithread.Leave() };
        }
    }
}

/// The caller's device, which every delivered frame lives on.
struct Shared {
    device: Device,
    luid: LUID,
    feature_level_11: bool,
    pixel_format: PixelFormat,
    staging: Arc<StagingTexturePool>,
    frames: Arc<TrimPool>,
    cursor_in_image: Arc<AtomicBool>,
}

impl Shared {
    fn new(
        device: ID3D11Device,
        pixel_format: PixelFormat,
        cursor_in_image: Arc<AtomicBool>,
    ) -> windows::core::Result<Self> {
        let luid = unsafe {
            device
                .cast::<IDXGIDevice>()?
                .GetAdapter()?
                .GetDesc()?
                .AdapterLuid
        };
        let feature_level_11 = unsafe { device.GetFeatureLevel() }.0 >= D3D_FEATURE_LEVEL_11_0.0;
        let staging = Arc::new(StagingTexturePool::new(device.clone(), pixel_format));
        Ok(Self {
            device: Device::new(device)?,
            luid,
            feature_level_11,
            pixel_format,
            staging,
            frames: Arc::new(TrimPool::with_bind_flags(
                D3D11_BIND_UNORDERED_ACCESS.0 as u32,
            )),
            cursor_in_image,
        })
    }

    fn is_removed(&self) -> bool {
        unsafe { self.device.device.GetDeviceRemovedReason() }.is_err()
    }
}

enum Conversion {
    Gpu(ID3D11ComputeShader),
    Cpu {
        staging: Option<(ID3D11Texture2D, u32, u32)>,
        pixels: Vec<u8>,
    },
}

/// One open duplication of the display. Dropped and reopened whenever
/// Windows takes the duplication away.
struct Session {
    /// The caller's device when it drives the display, otherwise one made on
    /// the display's adapter: a duplication only works there.
    device: Option<Device>,
    duplication: IDXGIOutputDuplication,
    desktop: RECT,
    conversion: Conversion,
    draw_cursor: bool,
    snapshot: Option<Snapshot>,
    cursor: Option<(usize, POINT)>,
    cursor_failed: bool,
    /// Watches for a software cursor while the cursor should stay out of
    /// the frames; `None` once found, or when the cursor is drawn anyway.
    pointer_probe: Option<PointerProbe>,
}

/// A hardware pointer moving over the display produces frames carrying
/// pointer updates; a software one is part of the desktop image and
/// produces none. Many moves in a row without a single pointer update mean
/// Windows is painting the pointer into the image.
#[derive(Default)]
struct PointerProbe {
    last_position: Option<POINT>,
    unreported_moves: u32,
}

impl PointerProbe {
    const SOFTWARE_AFTER_MOVES: u32 = 30;

    /// True once the pointer is known to be painted into the image.
    fn observe(&mut self, info: &DXGI_OUTDUPL_FRAME_INFO, desktop: &RECT) -> bool {
        if info.LastMouseUpdateTime != 0 {
            self.unreported_moves = 0;
            self.last_position = None;
            return false;
        }
        let mut cursor = CURSORINFO {
            cbSize: std::mem::size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        let on_display = unsafe { GetCursorInfo(&mut cursor) }.is_ok()
            && cursor.flags.0 & CURSOR_SHOWING.0 != 0
            && (desktop.left..desktop.right).contains(&cursor.ptScreenPos.x)
            && (desktop.top..desktop.bottom).contains(&cursor.ptScreenPos.y);
        if !on_display {
            self.last_position = None;
            return false;
        }
        if self
            .last_position
            .is_some_and(|last| last != cursor.ptScreenPos)
        {
            self.unreported_moves += 1;
        }
        self.last_position = Some(cursor.ptScreenPos);
        self.unreported_moves >= Self::SOFTWARE_AFTER_MOVES
    }
}

/// The duplicated region, copied out so the desktop frame can go back to
/// Windows straight away. GDI-compatible so the cursor can be drawn on it.
struct Snapshot {
    texture: ID3D11Texture2D,
    view: Option<ID3D11ShaderResourceView>,
    width: u32,
    height: u32,
}

impl Session {
    fn open(
        monitor: HMONITOR,
        settings: &Settings,
        shared: &Shared,
    ) -> Result<Self, NewDuplicationError> {
        let (adapter, output, desktop) = find_output(monitor)?;
        let adapter_luid = unsafe { adapter.GetDesc1() }?.AdapterLuid;
        let same_adapter = adapter_luid.LowPart == shared.luid.LowPart
            && adapter_luid.HighPart == shared.luid.HighPart;

        let device = if same_adapter {
            None
        } else {
            Some(Device::create_on(&adapter).map_err(NewDuplicationError::CreateDevice)?)
        };
        let capture_device = device.as_ref().unwrap_or(&shared.device);

        let duplication = duplicate(&output, &capture_device.device)?;
        let rotation = unsafe { duplication.GetDesc() }.Rotation;
        if rotation != DXGI_MODE_ROTATION_IDENTITY && rotation != DXGI_MODE_ROTATION_UNSPECIFIED {
            return Err(NewDuplicationError::RotatedOutput);
        }

        let conversion = match (same_adapter, shared.pixel_format, shared.feature_level_11) {
            (true, PixelFormat::R8G8B8A8Unorm, true) => match convert_shader(&shared.device.device)
            {
                Some(shader) => Conversion::Gpu(shader),
                None => Conversion::cpu(),
            },
            _ => Conversion::cpu(),
        };

        let draw_cursor = settings.is_cursor_capture_enabled.unwrap_or(true);
        let session = Self {
            device,
            duplication,
            desktop,
            conversion,
            draw_cursor,
            snapshot: None,
            cursor: None,
            cursor_failed: false,
            pointer_probe: (!draw_cursor && !shared.cursor_in_image.load(Ordering::Relaxed))
                .then(PointerProbe::default),
        };
        tracing::info!(
            path = ?session.path(),
            desktop = ?(desktop.left, desktop.top, desktop.right, desktop.bottom),
            draw_cursor = session.draw_cursor,
            "scap-direct3d: Desktop Duplication opened"
        );
        Ok(session)
    }

    fn path(&self) -> DuplicationPath {
        match self.conversion {
            Conversion::Gpu(_) => DuplicationPath::Gpu,
            Conversion::Cpu { .. } => DuplicationPath::Cpu,
        }
    }

    fn capture_device<'a>(&'a self, shared: &'a Shared) -> &'a Device {
        self.device.as_ref().unwrap_or(&shared.device)
    }

    /// `Ok(None)` when there is nothing new to deliver; `Err` when the
    /// duplication has to be reopened.
    fn next_frame(
        &mut self,
        shared: &Shared,
        settings: &Settings,
        force: bool,
    ) -> windows::core::Result<Option<Frame>> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match unsafe {
            self.duplication
                .AcquireNextFrame(ACQUIRE_TIMEOUT_MS, &mut info, &mut resource)
        } {
            Ok(()) => {}
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
            Err(error) => return Err(error),
        }

        if let Some(probe) = self.pointer_probe.as_mut()
            && probe.observe(&info, &self.desktop)
        {
            self.pointer_probe = None;
            if !shared.cursor_in_image.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    "scap-direct3d: Windows paints the pointer into the desktop image (software cursor); duplicated frames show it"
                );
            }
        }

        let wanted = force
            || info.LastPresentTime != 0
            || (self.draw_cursor && info.LastMouseUpdateTime != 0);
        let copied = match resource {
            Some(resource) if wanted => self.copy_desktop(shared, settings, &resource),
            _ => Ok(None),
        };
        let released = unsafe { self.duplication.ReleaseFrame() };
        let region = copied?;
        released?;
        let Some(region) = region else {
            return Ok(None);
        };

        if self.draw_cursor {
            self.draw_cursor(shared, &region);
        }

        let capture_time = if info.LastPresentTime != 0 {
            performance_counter_to_100ns(info.LastPresentTime)
        } else {
            now_100ns()
        };
        self.convert(shared, capture_time).map(Some)
    }

    /// Copies the region to record into the snapshot. `Ok(None)` when the
    /// desktop no longer contains it (a resolution change shrank it).
    fn copy_desktop(
        &mut self,
        shared: &Shared,
        settings: &Settings,
        resource: &IDXGIResource,
    ) -> windows::core::Result<Option<D3D11_BOX>> {
        let desktop: ID3D11Texture2D = resource.cast()?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { desktop.GetDesc(&mut desc) };
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            tracing::warn!(format = ?desc.Format, "scap-direct3d: unexpected desktop format, frame skipped");
            return Ok(None);
        }

        let region = match settings.crop {
            Some(crop) => {
                if crop.right > desc.Width
                    || crop.bottom > desc.Height
                    || crop.right <= crop.left
                    || crop.bottom <= crop.top
                {
                    return Ok(None);
                }
                crop
            }
            None => {
                let (mut width, mut height) = (desc.Width, desc.Height);
                if settings.even_dimensions {
                    width &= !1;
                    height &= !1;
                }
                if width == 0 || height == 0 {
                    return Ok(None);
                }
                D3D11_BOX {
                    left: 0,
                    top: 0,
                    front: 0,
                    right: width,
                    bottom: height,
                    back: 1,
                }
            }
        };
        let width = region.right - region.left;
        let height = region.bottom - region.top;

        let gpu = matches!(self.conversion, Conversion::Gpu(_));
        if self
            .snapshot
            .as_ref()
            .is_none_or(|snapshot| (snapshot.width, snapshot.height) != (width, height))
        {
            let device = self.capture_device(shared);
            let texture = create_texture(
                &device.device,
                width,
                height,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                D3D11_USAGE_DEFAULT.0,
                (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                0,
                D3D11_RESOURCE_MISC_GDI_COMPATIBLE.0 as u32,
            )?;
            let view = if gpu {
                let mut view = None;
                unsafe {
                    device
                        .device
                        .CreateShaderResourceView(&texture, None, Some(&mut view))
                }?;
                view
            } else {
                None
            };
            self.snapshot = Some(Snapshot {
                texture,
                view,
                width,
                height,
            });
        }

        let device = self.capture_device(shared);
        let snapshot = self.snapshot.as_ref().expect("snapshot created above");
        let _lock = device.lock();
        unsafe {
            device.context.CopySubresourceRegion(
                &snapshot.texture,
                0,
                0,
                0,
                0,
                &desktop,
                0,
                Some(&region),
            );
        }
        Ok(Some(region))
    }

    /// Best effort: a frame without the cursor beats no frame.
    fn draw_cursor(&mut self, shared: &Shared, region: &D3D11_BOX) {
        if self.cursor_failed {
            return;
        }
        let mut info = CURSORINFO {
            cbSize: std::mem::size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetCursorInfo(&mut info) }.is_err()
            || info.flags.0 & CURSOR_SHOWING.0 == 0
            || info.hCursor.0.is_null()
        {
            return;
        }
        let hotspot = self.cursor_hotspot(info.hCursor);
        let x = info.ptScreenPos.x - hotspot.x - self.desktop.left - region.left as i32;
        let y = info.ptScreenPos.y - hotspot.y - self.desktop.top - region.top as i32;

        let Some(snapshot) = self.snapshot.as_ref() else {
            return;
        };
        let result = (|| -> windows::core::Result<()> {
            let surface: IDXGISurface1 = snapshot.texture.cast()?;
            let _lock = self.capture_device(shared).lock();
            let hdc = unsafe { surface.GetDC(false) }?;
            let drawn =
                unsafe { DrawIconEx(hdc, x, y, HICON(info.hCursor.0), 0, 0, 0, None, DI_NORMAL) };
            unsafe { surface.ReleaseDC(None) }?;
            drawn
        })();
        if let Err(error) = result {
            tracing::warn!(%error, "scap-direct3d: can't draw the cursor into duplicated frames");
            self.cursor_failed = true;
        }
    }

    fn cursor_hotspot(&mut self, cursor: HCURSOR) -> POINT {
        let key = cursor.0 as usize;
        if let Some((cached, hotspot)) = self.cursor
            && cached == key
        {
            return hotspot;
        }
        let mut icon = ICONINFO::default();
        let hotspot = if unsafe { GetIconInfo(HICON(cursor.0), &mut icon) }.is_ok() {
            unsafe {
                if !icon.hbmMask.0.is_null() {
                    let _ = DeleteObject(HGDIOBJ(icon.hbmMask.0));
                }
                if !icon.hbmColor.0.is_null() {
                    let _ = DeleteObject(HGDIOBJ(icon.hbmColor.0));
                }
            }
            POINT {
                x: icon.xHotspot as i32,
                y: icon.yHotspot as i32,
            }
        } else {
            POINT::default()
        };
        self.cursor = Some((key, hotspot));
        hotspot
    }

    fn convert(&mut self, shared: &Shared, capture_time: i64) -> windows::core::Result<Frame> {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Err(windows::core::Error::new(E_FAIL, "no snapshot"));
        };
        let (width, height) = (snapshot.width, snapshot.height);
        let texture =
            shared
                .frames
                .take(&shared.device.device, width, height, shared.pixel_format)?;

        match &mut self.conversion {
            Conversion::Gpu(shader) => {
                let device = &shared.device;
                let mut target: Option<ID3D11UnorderedAccessView> = None;
                unsafe {
                    device
                        .device
                        .CreateUnorderedAccessView(&texture, None, Some(&mut target))
                }?;
                let _lock = device.lock();
                unsafe {
                    let context = &device.context;
                    context.CSSetShader(&*shader, None);
                    context.CSSetShaderResources(0, Some(&[snapshot.view.clone()]));
                    context.CSSetUnorderedAccessViews(0, 1, Some(&target as *const _), None);
                    context.Dispatch(width.div_ceil(8), height.div_ceil(8), 1);
                    let unbound: Option<ID3D11UnorderedAccessView> = None;
                    context.CSSetUnorderedAccessViews(0, 1, Some(&unbound as *const _), None);
                    context.CSSetShaderResources(0, Some(&[None]));
                    context.CSSetShader(None::<&ID3D11ComputeShader>, None);
                }
            }
            Conversion::Cpu { staging, pixels } => {
                let capture_device = self.device.as_ref().unwrap_or(&shared.device);
                if staging
                    .as_ref()
                    .is_none_or(|(_, w, h)| (*w, *h) != (width, height))
                {
                    let texture = create_texture(
                        &capture_device.device,
                        width,
                        height,
                        DXGI_FORMAT_B8G8R8A8_UNORM,
                        D3D11_USAGE_STAGING.0,
                        0,
                        D3D11_CPU_ACCESS_READ.0 as u32,
                        0,
                    )?;
                    *staging = Some((texture, width, height));
                }
                let (staging_texture, _, _) = staging.as_ref().expect("staging created above");

                let row = width as usize * 4;
                pixels.resize(row * height as usize, 0);
                {
                    let _lock = capture_device.lock();
                    let context = &capture_device.context;
                    unsafe { context.CopyResource(staging_texture, &snapshot.texture) };
                    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                    unsafe {
                        context.Map(staging_texture, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                    }?;
                    let source = unsafe {
                        std::slice::from_raw_parts(
                            mapped.pData.cast::<u8>(),
                            mapped.RowPitch as usize * (height as usize - 1) + row,
                        )
                    };
                    convert_rows(
                        source,
                        mapped.RowPitch as usize,
                        pixels,
                        row,
                        shared.pixel_format,
                    );
                    unsafe { context.Unmap(staging_texture, 0) };
                }

                let _lock = shared.device.lock();
                unsafe {
                    shared.device.context.UpdateSubresource(
                        &texture,
                        0,
                        None,
                        pixels.as_ptr().cast(),
                        row as u32,
                        0,
                    );
                }
            }
        }

        Ok(Frame {
            width,
            height,
            pixel_format: shared.pixel_format,
            inner: None,
            capture_time,
            texture,
            d3d_device: shared.device.device.clone(),
            d3d_context: shared.device.context.clone(),
            staging_pool: shared.staging.clone(),
            recycle: Some(shared.frames.clone()),
        })
    }
}

impl Conversion {
    fn cpu() -> Self {
        Self::Cpu {
            staging: None,
            pixels: Vec::new(),
        }
    }
}

/// BGRA rows from the desktop into tightly packed rows of `format`, opaque.
fn convert_rows(
    source: &[u8],
    source_pitch: usize,
    target: &mut [u8],
    row: usize,
    format: PixelFormat,
) {
    for (index, target_row) in target.chunks_exact_mut(row).enumerate() {
        let start = index * source_pitch;
        let source_row = &source[start..start + row];
        for (to, from) in target_row
            .chunks_exact_mut(4)
            .zip(source_row.chunks_exact(4))
        {
            match format {
                PixelFormat::R8G8B8A8Unorm => {
                    to[0] = from[2];
                    to[1] = from[1];
                    to[2] = from[0];
                }
                PixelFormat::B8G8R8A8Unorm => to[..3].copy_from_slice(&from[..3]),
            }
            to[3] = u8::MAX;
        }
    }
}

fn find_output(
    monitor: HMONITOR,
) -> Result<(IDXGIAdapter1, IDXGIOutput1, RECT), NewDuplicationError> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;
    for adapter_index in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(adapter_index) }) else {
            break;
        };
        for output_index in 0.. {
            let Ok(output) = (unsafe { adapter.EnumOutputs(output_index) }) else {
                break;
            };
            let desc = unsafe { output.GetDesc() }?;
            if desc.Monitor == monitor {
                return Ok((adapter, output.cast()?, desc.DesktopCoordinates));
            }
        }
    }
    Err(NewDuplicationError::OutputNotFound)
}

/// `DuplicateOutput1` hands an HDR or wide-gamut desktop over converted to
/// 8-bit BGRA; the older call is the fallback where it is missing.
fn duplicate(
    output: &IDXGIOutput1,
    device: &ID3D11Device,
) -> Result<IDXGIOutputDuplication, NewDuplicationError> {
    if let Ok(output) = output.cast::<IDXGIOutput5>()
        && let Ok(duplication) =
            unsafe { output.DuplicateOutput1(device, 0, &[DXGI_FORMAT_B8G8R8A8_UNORM]) }
    {
        return Ok(duplication);
    }
    unsafe { output.DuplicateOutput(device) }.map_err(NewDuplicationError::DuplicateOutput)
}

#[allow(clippy::too_many_arguments)]
fn create_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT,
    usage: i32,
    bind_flags: u32,
    cpu_access: u32,
    misc_flags: u32,
) -> windows::core::Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: windows::Win32::Graphics::Direct3D11::D3D11_USAGE(usage),
        BindFlags: bind_flags,
        CPUAccessFlags: cpu_access,
        MiscFlags: misc_flags,
    };
    let mut texture = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }?;
    texture.ok_or_else(windows::core::Error::from_win32)
}

fn convert_shader(device: &ID3D11Device) -> Option<ID3D11ComputeShader> {
    static BYTECODE: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    let bytecode = BYTECODE.get_or_init(compile_convert_shader).as_deref()?;
    let mut shader = None;
    match unsafe { device.CreateComputeShader(bytecode, None, Some(&mut shader)) } {
        Ok(()) => shader,
        Err(error) => {
            tracing::warn!(%error, "scap-direct3d: GPU conversion unavailable, converting on the CPU");
            None
        }
    }
}

type D3DCompileFn = unsafe extern "system" fn(
    source: *const c_void,
    source_len: usize,
    source_name: PCSTR,
    defines: *const c_void,
    include: *mut c_void,
    entry_point: PCSTR,
    target: PCSTR,
    flags1: u32,
    flags2: u32,
    code: *mut *mut c_void,
    errors: *mut *mut c_void,
) -> HRESULT;

/// The compiler is loaded on first use rather than linked, so the app never
/// fails to start over it; without it frames are converted on the CPU.
fn compile_convert_shader() -> Option<Vec<u8>> {
    unsafe {
        let module = LoadLibraryExW(w!("d3dcompiler_47.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
            .inspect_err(
                |error| tracing::warn!(%error, "scap-direct3d: d3dcompiler_47.dll unavailable"),
            )
            .ok()?;
        let compile: D3DCompileFn = std::mem::transmute(GetProcAddress(module, s!("D3DCompile"))?);

        let mut code = null_mut();
        let mut errors = null_mut();
        let result = compile(
            CONVERT_SHADER.as_ptr().cast(),
            CONVERT_SHADER.len(),
            s!("duplication-convert.hlsl"),
            null(),
            null_mut(),
            s!("main"),
            s!("cs_5_0"),
            0,
            0,
            &mut code,
            &mut errors,
        );
        let errors = (!errors.is_null()).then(|| ID3DBlob::from_raw(errors));
        let code = (!code.is_null()).then(|| ID3DBlob::from_raw(code));
        match code {
            Some(code) if result.is_ok() => Some(
                std::slice::from_raw_parts(
                    code.GetBufferPointer().cast::<u8>(),
                    code.GetBufferSize(),
                )
                .to_vec(),
            ),
            _ => {
                let message = errors
                    .map(|blob| {
                        String::from_utf8_lossy(std::slice::from_raw_parts(
                            blob.GetBufferPointer().cast::<u8>(),
                            blob.GetBufferSize(),
                        ))
                        .into_owned()
                    })
                    .unwrap_or_default();
                tracing::warn!(hresult = ?result, %message, "scap-direct3d: conversion shader didn't compile");
                None
            }
        }
    }
}

fn performance_counter_frequency() -> i64 {
    static FREQUENCY: OnceLock<i64> = OnceLock::new();
    *FREQUENCY.get_or_init(|| {
        let mut frequency = 0;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut frequency);
        }
        frequency.max(1)
    })
}

/// The unit of WGC's `SystemRelativeTime`, which `Frame::capture_time` uses.
fn performance_counter_to_100ns(ticks: i64) -> i64 {
    (ticks as i128 * 10_000_000 / performance_counter_frequency() as i128) as i64
}

fn now_100ns() -> i64 {
    let mut ticks = 0;
    unsafe {
        let _ = QueryPerformanceCounter(&mut ticks);
    }
    performance_counter_to_100ns(ticks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_become_opaque_rgba() {
        // Two BGRA pixels per row, a pitch with two bytes of padding.
        let source = [
            1, 2, 3, 0, 4, 5, 6, 7, 0xAA, 0xAA, //
            8, 9, 10, 11, 12, 13, 14, 15, 0xAA, 0xAA,
        ];
        let mut target = vec![0; 16];
        convert_rows(&source, 10, &mut target, 8, PixelFormat::R8G8B8A8Unorm);
        assert_eq!(
            target,
            [3, 2, 1, 255, 6, 5, 4, 255, 10, 9, 8, 255, 14, 13, 12, 255]
        );

        convert_rows(&source, 10, &mut target, 8, PixelFormat::B8G8R8A8Unorm);
        assert_eq!(
            target,
            [1, 2, 3, 255, 4, 5, 6, 255, 8, 9, 10, 255, 12, 13, 14, 255]
        );
    }

    #[test]
    fn performance_counter_converts_to_100ns() {
        let frequency = performance_counter_frequency();
        assert_eq!(performance_counter_to_100ns(frequency), 10_000_000);
        assert_eq!(performance_counter_to_100ns(0), 0);
    }
}
