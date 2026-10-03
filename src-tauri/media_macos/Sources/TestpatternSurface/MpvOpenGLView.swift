import AppKit
import OpenGL.GL

/// An NSOpenGLView subclass mpv renders into. The drawRect: override makes
/// the CGL context current, asks mpv for the frame (through a Rust-registered
/// callback), flushes. Rust installs the callback pointer once at init.
///
/// This is Swift rather than Rust because a `drawRect:` override is one of
/// the few things objc2's `declare_class!` still handles awkwardly; the file
/// stays tiny on purpose.
public final class MpvOpenGLView: NSOpenGLView {

  /// Rust side sets this to its `render_frame(view)`: draws a fresh frame into
  /// the (already current) CGL context; draw(_:) then calls flushBuffer().
  public typealias RenderFn = @convention(c) (UnsafeMutableRawPointer?) -> Void
  private static var renderFn: RenderFn?

  public override func draw(_ dirtyRect: NSRect) {
    guard let ctx = openGLContext else { return }
    ctx.makeCurrentContext()
    let cgl = ctx.cglContextObj!
    CGLLockContext(cgl)
    if let render = MpvOpenGLView.renderFn {
      render(Unmanaged.passUnretained(self).toOpaque())
    } else {
      glClearColor(0, 0, 0, 1)
      glClear(GLbitfield(GL_COLOR_BUFFER_BIT))
    }
    ctx.flushBuffer()
    CGLUnlockContext(cgl)
  }

  public override func reshape() {
    super.reshape()
    // A resize can drop a pending frame; asking for a redraw keeps mpv's
    // output tracking the window without waiting for the next packet.
    needsDisplay = true
  }

  /// Called by Rust right after the view is created.
  @objc public static func setRenderCallback(_ fn: RenderFn?) {
    renderFn = fn
  }

  /// Pixel format for the class's view: 3.2 Core, double-buffered. Kept in
  /// Swift because the objc2 Rust binding of `initWithAttributes:` goes
  /// through a calling convention that delivers nil.
  @objc(tp_mpvPixelFormat) public static func mpvPixelFormat() -> NSOpenGLPixelFormat? {
    let attrs: [NSOpenGLPixelFormatAttribute] = [
      NSOpenGLPixelFormatAttribute(NSOpenGLPFAOpenGLProfile), NSOpenGLPixelFormatAttribute(NSOpenGLProfileVersion3_2Core),
      NSOpenGLPixelFormatAttribute(NSOpenGLPFADoubleBuffer),
      0,
    ]
    return NSOpenGLPixelFormat(attributes: attrs)
  }
}
