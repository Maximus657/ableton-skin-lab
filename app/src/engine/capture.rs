//! Windows Graphics Capture of Live's main window. Frames arrive on a thread-pool thread, which
//! only signals an event; the engine's own thread picks the newest frame up.

use super::gpu::Gpu;
use windows::core::{Interface, Result};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::System::Threading::SetEvent;
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

pub struct Capture {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    device: IDirect3DDevice,
    size: SizeInt32,
}

impl Capture {
    pub fn new(device: &IDirect3DDevice, hwnd: HWND, frame_event: HANDLE) -> Result<Capture> {
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(hwnd)? };
        let size = item.Size()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(device, DirectXPixelFormat::B8G8R8A8UIntNormalized, 2, size)?;
        let event = frame_event.0 as isize;
        pool.FrameArrived(&TypedEventHandler::new(move |_, _| {
            unsafe {
                let _ = SetEvent(HANDLE(event as _));
            }
            Ok(())
        }))?;
        let session = pool.CreateCaptureSession(&item)?;
        let _ = session.SetIsCursorCaptureEnabled(false);
        // no yellow frame around Live (Windows 11)
        let _ = session.SetIsBorderRequired(false);
        session.StartCapture()?;
        Ok(Capture { pool, session, device: device.clone(), size })
    }

    /// Copies the newest frame, if one arrived, into the GPU's frame texture.
    /// Returns the frame size when a frame was taken.
    pub fn take(&mut self, gpu: &mut Gpu) -> Option<(u32, u32)> {
        let mut newest = None;
        while let Ok(f) = self.pool.TryGetNextFrame() {
            newest = Some(f);
        }
        let frame = newest?;
        let cs = frame.ContentSize().ok()?;
        let (w, h) = (cs.Width.max(0) as u32, cs.Height.max(0) as u32);
        let tex: Option<ID3D11Texture2D> = frame
            .Surface()
            .and_then(|s| s.cast::<IDirect3DDxgiInterfaceAccess>())
            .and_then(|a| unsafe { a.GetInterface() })
            .ok();
        let taken = tex.map(|t| gpu.take_frame(&t, w, h).unwrap_or(false)).unwrap_or(false);
        let _ = frame.Close();
        if cs.Width != self.size.Width || cs.Height != self.size.Height {
            // the window was resized: the pool's buffers follow
            self.size = cs;
            let _ = self.pool.Recreate(&self.device, DirectXPixelFormat::B8G8R8A8UIntNormalized, 2, cs);
        }
        taken.then_some((w, h))
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
