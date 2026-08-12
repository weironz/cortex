; ══════════════════════════════════════════════════════════
;  scripts/windows/cortex.iss —— Cortex 桌面端的 Windows 安装程序
;
;  不要直接用 IDE 编译它：全部路径都由 scripts/release-desktop-windows.sh
;  经 /D 传进来（那个脚本还负责补 MSVC 运行库并在打包前真的启动一次 GUI）。
;
;  ── 这个文件必须是 UTF-8 with BOM ─────────────────────────
;  Inno Setup 6 读没有 BOM 的脚本时按系统 ANSI 代码页解释。中文机器上
;  碰巧显示正常，CI 的英文机器上就是一屏乱码 —— 而它编译得过、
;  安装也成功，只有那几页字是坏的。
;
;  ── 为什么向导本身是英文 ──────────────────────────────────
;  Inno Setup 官方安装包里**没有**简体中文语言文件（Languages/ 目录里
;  29 个 .isl，没有 ChineseSimplified）。要中文向导就得把非官方翻译
;  vendored 进仓库，那会多一份需要在 NOTICE 里交代出处的第三方文件，
;  只为几个按钮的字面。所以：向导框架用英文，**我们自己的话**
;  （许可协议、装的是什么、SmartScreen 怎么办）全部中文，
;  写在 InfoBefore 页与安装目录里的 README.txt。
; ══════════════════════════════════════════════════════════

#ifndef AppVersion
  #error 必须由 release-desktop-windows.sh 传入 /DAppVersion=…
#endif

[Setup]
; AppId 是**永久**的：Windows 靠它认出「这是同一个程序的新版本」而不是
; 第二份并存的安装。改了它，用户机器上会出现两个 Cortex，
; 而旧的那个再也不会被新版本升级掉。生成一次，此后不动。
AppId={{C4F4EE25-DF07-422F-9054-89D06CAD30CD}
AppName=Cortex
AppVersion={#AppVersion}
AppVerName=Cortex {#AppVersion}
AppPublisher=Cortex
AppPublisherURL=https://github.com/weironz/cortex
AppSupportURL=https://github.com/weironz/cortex/issues
AppUpdatesURL=https://github.com/weironz/cortex/releases
VersionInfoVersion={#AppVersion}
VersionInfoProductName=Cortex
VersionInfoDescription=Cortex 桌面端安装程序

; ── 装在用户目录，不要管理员权限 ──────────────────────────
;
; 这份安装程序**没有代码签名**（理由见 CHANGELOG）。不签名 + 要求提权
; 意味着用户要连过两关：先是 SmartScreen 的蓝屏，再是 UAC 那个红底的
; 「未知发布者」。而这个程序装进 %LOCALAPPDATA%\Programs 完全够用 ——
; 它不注册服务、不写 HKLM、不装驱动，没有任何需要管理员的动作。
;
; 于是 PrivilegesRequired=lowest：全程零 UAC，用户只需要判断一次
; （SmartScreen 那一次），而不是两次。少一个吓人的弹窗，
; 就少一个「这软件是不是有问题」的理由。
PrivilegesRequired=lowest
DefaultDirName={autopf}\Cortex
DefaultGroupName=Cortex
DisableProgramGroupPage=yes
AllowNoIcons=yes

; x64compatible 而不是 x64：ARM64 的 Windows 能用 x64 仿真跑这份产物。
; 32 位 Windows 则明确拒绝 —— Flutter 的 windows 产物只有 x64
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

LicenseFile={#LicenseFile}
InfoBeforeFile={#InfoFile}
OutputDir={#OutputDir}
OutputBaseFilename={#OutputBase}
SetupIconFile={#IconFile}
UninstallDisplayIcon={app}\cortex_app.exe
UninstallDisplayName=Cortex {#AppVersion}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; 装到一半发现磁盘不够，是一种完全可以提前避免的失败
DiskSpanning=no

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
; 整棵目录树（cortex_app.exe + flutter_windows.dll + 插件 DLL + data/ +
; 脚本补进去的三个 MSVC 运行库 + LICENSE / NOTICE / CHANGELOG / README）。
; 逐文件列出来的版本已经试过，每加一个插件就得改一次，
; 而漏掉一个的症状是运行期崩溃而不是编译期报错
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\Cortex"; Filename: "{app}\cortex_app.exe"
Name: "{group}\先读我（装的是什么）"; Filename: "{app}\README.txt"
Name: "{autodesktop}\Cortex"; Filename: "{app}\cortex_app.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\cortex_app.exe"; Description: "{cm:LaunchProgram,Cortex}"; Flags: nowait postinstall skipifsilent
Filename: "{app}\README.txt"; Description: "看一眼「装的是什么」（它需要一台 cortexd）"; Flags: shellexec postinstall skipifsilent unchecked

; ── 自动更新装完之后，谁负责把应用重新拉起来 ──────────────
;
; 上面那条带 `postinstall skipifsilent`：它是安装向导最后那个「立即运行」
; 复选框，**静默安装时不执行**。而自动更新走的正是 `/VERYSILENT` ——
; 只有上面那条的话，用户点一下「更新」，应用消失，再也不回来。
;
; 主路径其实是 Restart Manager：更新器传了 `/CLOSEAPPLICATIONS`
; （RM 去关掉占着文件的 cortex_app.exe）与 `/RESTARTAPPLICATIONS`
; （装完把它关掉的那些再拉起来）。
;
; 这一条是**兜底**：RM 只重启注册过的进程，拉不起来的情况是存在的，
; 而那种情况没有任何提示 —— 用户只看到应用没了。
;
; `Check: WizardSilent` 保证它**只在静默安装时**生效，与上面那条
; `skipifsilent` 正好互补，两条永远只有一条会跑。少了这个 Check，
; 正常安装会把应用拉起来两次。
Filename: "{app}\cortex_app.exe"; Flags: nowait; Check: WizardSilent

[UninstallDelete]
; Inno 只删自己装过的文件。Flutter 的 data/ 下面 Dart 运行时会留下
; 一些安装清单里没有的东西（着色器缓存之类），不显式清掉的话
; 卸载完 %LOCALAPPDATA%\Programs\Cortex 会留一棵空壳目录树。
; 只删安装目录本身，用户数据在 %APPDATA% 里，不碰
Type: filesandordirs; Name: "{app}"
