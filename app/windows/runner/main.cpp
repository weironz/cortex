#include <flutter/dart_project.h>
#include <flutter/flutter_view_controller.h>
#include <windows.h>

#include "flutter_window.h"
#include "utils.h"

int APIENTRY wWinMain(_In_ HINSTANCE instance, _In_opt_ HINSTANCE prev,
                      _In_ wchar_t *command_line, _In_ int show_command) {
  // Attach to console when present (e.g., 'flutter run') or create a
  // new console when running with a debugger.
  if (!::AttachConsole(ATTACH_PARENT_PROCESS) && ::IsDebuggerPresent()) {
    CreateAndAttachConsole();
  }

  // Initialize COM, so that it is available for use in the library and/or
  // plugins.
  ::CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);

  flutter::DartProject project(L"data");

  std::vector<std::string> command_line_arguments =
      GetCommandLineArguments();

  project.set_dart_entrypoint_arguments(std::move(command_line_arguments));

  FlutterWindow window(project);
  Win32Window::Point origin(10, 10);
  Win32Window::Size size(1280, 720);
  // 这个文件必须保持 UTF-8 with BOM：runner 是用 /W4 /WX 编的，没有 BOM 时
  // MSVC 按系统代码页读它，非 ASCII 字符触发 C4819，而 /WX 把它变成硬错误。
  // 中文机器（936）与 CI 的英文机器（1252）都会挂，症状是编译失败而不是乱码。
  //
  // 窗口标题就是任务栏、Alt-Tab 与「未响应」对话框上显示的那个名字。
  // 模板给的 "cortex_app" 是 pubspec 的包名，装完之后用户在任务栏上看到的
  // 就是它 —— 一个带安装程序的产品不该用内部标识符自称
  if (!window.Create(L"Cortex", origin, size)) {
    return EXIT_FAILURE;
  }
  window.SetQuitOnClose(true);

  ::MSG msg;
  while (::GetMessage(&msg, nullptr, 0, 0)) {
    ::TranslateMessage(&msg);
    ::DispatchMessage(&msg);
  }

  ::CoUninitialize();
  return EXIT_SUCCESS;
}
