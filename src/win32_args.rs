//! Windows の API（stdcall）の引数の個数。呼ばれた側が引数を片付ける（ret N）ので、個数が違うとスタックがずれて戻り先が壊れる。
//! 値は Microsoft が公開している各関数の仕様どおり。載っていない API は 0 個として扱う（host_stubs.rs の arg_count が、ここを引く）

/// GUI まわり（ole32、USER32、GDI32、SHELL32）。エディターを開かない限りほとんど呼ばれないが、初期化で OleInitialize などが呼ばれる
pub fn gui_arg_count(name: &str) -> Option<u64> {
    Some(match name {
        // ole32
        "OleInitialize" | "RevokeDragDrop" => 1,
        "OleUninitialize" => 0,
        "RegisterDragDrop" => 2,
        "CoCreateInstance" => 5,
        // SHELL32
        "DragQueryFileA" => 4,
        // GDI32
        "CreateBrushIndirect" | "CreateCompatibleDC" | "CreateFontIndirectA" | "CreatePenIndirect" | "CreateSolidBrush" | "DeleteDC" | "DeleteObject" | "GetStockObject" => 1,
        "GetCurrentObject" | "SelectObject" | "SetBkColor" | "SetBkMode" | "SetTextColor" => 2,
        "CreateCompatibleBitmap" | "DPtoLP" | "GetObjectA" | "LineTo" => 3,
        "MoveToEx" => 4,
        "CreateBitmap" => 5,
        "BitBlt" => 9,
        // USER32
        "DestroyWindow" | "DispatchMessageA" | "GetAsyncKeyState" | "GetCursorPos" | "GetDC" | "GetSysColorBrush" | "GetSystemMetrics" | "RegisterClassA" | "SetCursor" | "ShowCursor" | "GetParent" => 1,
        "GetDoubleClickTime" => 0,
        "BeginPaint" | "EndPaint" | "GetWindowLongA" | "GetWindowRect" | "LoadBitmapA" | "LoadCursorA" | "ReleaseDC" | "SetCursorPos" | "UnregisterClassA" => 2,
        "FillRect" | "GetUpdateRect" | "SetWindowLongA" => 3,
        "DefWindowProcA" | "MapWindowPoints" => 4,
        "DrawTextA" | "PeekMessageA" => 5,
        "CreateWindowExA" => 12,
        // MSIMG32
        "TransparentBlt" | "AlphaBlend" => 11,
        _ => return None,
    })
}
