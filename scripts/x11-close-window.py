#!/usr/bin/env python3
"""Asks an X11 window to close the way a window manager does (WM_DELETE_WINDOW).

    scripts/x11-close-window.py WINDOW_ID     (decimal, as printed by `xdotool search`)

The e2e scripts run under Xvfb without a window manager, and `xdotool windowclose` destroys the
window instead of asking it, so the client's close handling (safe close of unsaved drafts) would
never run. This sends the ICCCM WM_PROTOCOLS / WM_DELETE_WINDOW client message through libX11
(ctypes only; no extra packages). Test tooling, not shipped.
"""
import ctypes
import ctypes.util
import sys

CLIENT_MESSAGE = 33
NO_EVENT_MASK = 0


class ClientMessage(ctypes.Structure):
    _fields_ = [
        ('type', ctypes.c_int),
        ('serial', ctypes.c_ulong),
        ('send_event', ctypes.c_int),
        ('display', ctypes.c_void_p),
        ('window', ctypes.c_ulong),
        ('message_type', ctypes.c_ulong),
        ('format', ctypes.c_int),
        ('data', ctypes.c_long * 5),
    ]


class XEvent(ctypes.Union):
    _fields_ = [('xclient', ClientMessage), ('pad', ctypes.c_long * 24)]


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__.strip().splitlines()[2].strip())
    window = int(sys.argv[1])
    xlib = ctypes.CDLL(ctypes.util.find_library('X11') or 'libX11.so.6')
    xlib.XOpenDisplay.restype = ctypes.c_void_p
    xlib.XOpenDisplay.argtypes = [ctypes.c_char_p]
    xlib.XInternAtom.restype = ctypes.c_ulong
    xlib.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
    xlib.XSendEvent.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_long,
                                ctypes.POINTER(XEvent)]
    xlib.XFlush.argtypes = [ctypes.c_void_p]
    xlib.XCloseDisplay.argtypes = [ctypes.c_void_p]
    display = xlib.XOpenDisplay(None)
    if not display:
        sys.exit('x11-close-window: cannot open the display')
    event = XEvent()
    event.xclient.type = CLIENT_MESSAGE
    event.xclient.window = window
    event.xclient.message_type = xlib.XInternAtom(display, b'WM_PROTOCOLS', 0)
    event.xclient.format = 32
    event.xclient.data[0] = xlib.XInternAtom(display, b'WM_DELETE_WINDOW', 0)
    event.xclient.data[1] = 0  # CurrentTime
    if not xlib.XSendEvent(display, window, 0, NO_EVENT_MASK, ctypes.byref(event)):
        sys.exit('x11-close-window: XSendEvent failed')
    xlib.XFlush(display)
    xlib.XCloseDisplay(display)


if __name__ == '__main__':
    main()
