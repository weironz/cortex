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
;
; 只删安装目录本身。用户数据在 **%LOCALAPPDATA%\cortex**（会话、附件、
; 工作区绑定、MCP 配置、启动记录），那一棵由下面 [Code] 里那一问决定 ——
; 默认保留，勾了才删。
Type: filesandordirs; Name: "{app}"

[Code]
// ══════════════════════════════════════════════════════════
//  卸载时问一句：本地数据要不要一起删
//
//  ── 为什么必须问，而不是默默留下 ──────────────────────────
//
//  「卸载之后留下了什么、在哪、多大」永远要有明文答案。默默留下 1 GB
//  数据的程序，用户是在几个月后清理磁盘时才发现的 —— 那时他已经不记得
//  Cortex 是什么，只看到一个来路不明的目录。
//
//  抄 Ollama 的卸载器：**把算出来的大小和完整路径摆在复选框上**。
//  不摆的话，「要不要删本地数据」对用户是一道没有信息的题。
//
//  ── 为什么默认不勾 ────────────────────────────────────────
//
//  卸载常常只是「重装一次试试」的中间步骤。默认删掉的话，那条常见路径
//  的代价是全部历史会话，而且不可撤销。**误留的代价是磁盘，误删的代价
//  是数据** —— 两边不对等时，选往能恢复那边倒的默认值。
//
//  ── 还装着 CLI 的人会看到它重新出现 ───────────────────────
//
//  这个目录是桌面端与 `cortex` CLI **共用**的。只卸桌面端而 CLI 还在，
//  删掉之后 CLI 下次跑起来会重新建。这句话必须写在弹窗上：否则用户会
//  以为「删了又回来」是这个卸载器没做干净。
// ══════════════════════════════════════════════════════════

const
  // 扫描条目上限。一个装了几万份附件的目录如果全量遍历，卸载器会**看起来
  // 卡死**几秒钟 —— 而它此刻正处在用户最没有耐心的时刻。到顶就停，
  // 显示成「≥ 多少」，一个诚实的下界胜过一次卡顿
  MaxScanEntries = 60000;

var
  DeleteUserData: Boolean;

function UserDataDir(): String;
begin
  Result := ExpandConstant('{localappdata}\cortex');
end;

function DirSize(const Path: String; var Entries: Integer): Int64;
var
  Rec: TFindRec;
begin
  Result := 0;
  if not FindFirst(AddBackslash(Path) + '*', Rec) then
    Exit;
  try
    repeat
      if (Rec.Name = '.') or (Rec.Name = '..') then
        Continue;
      Entries := Entries + 1;
      if Entries > MaxScanEntries then
        Exit;
      if (Rec.Attributes and FILE_ATTRIBUTE_DIRECTORY) <> 0 then
        Result := Result + DirSize(AddBackslash(Path) + Rec.Name, Entries)
      else
        // SizeHigh/SizeLow 而不是一个 Size 字段：Inno 的 TFindRec 就是
        // 这个形状。只取 SizeLow 的话，4 GB 以上的目录会报出一个小数字
        Result := Result + Int64(Rec.SizeHigh) * 4294967296 + Int64(Rec.SizeLow);
    until not FindNext(Rec);
  finally
    FindClose(Rec);
  end;
end;

// 两位小数**手算**，不走 `Format('%.2f')`。
//
// 那一版编译得过，运行期报「Format '%.2f GB' invalid or incompatible with
// argument」—— Inno 的 Pascal Script 把 `Int64 / 整数` 交给 `%f` 时对不上。
//
// 这个错在这里的代价远不止显示难看：它发生在 `CurUninstallStepChanged` 里，
// **Inno 把 CurUninstallStepChanged 抛出的异常当致命错误，整个卸载当场中止**
// —— 实测退出码 1，程序文件一个都没删。一句「把大小显示得好看点」
// 足以让这个产品卸载不掉。2026-08-20 用一个探针安装包实测到。
function Frac2(Remainder, Divisor: Int64): String;
var
  H: Int64;
begin
  // Remainder < Divisor <= 1 GB，乘 100 也就 1.1e11，Int64 装得下
  H := (Remainder * 100) div Divisor;
  if H < 10 then
    Result := '0' + IntToStr(H)
  else
    Result := IntToStr(H);
end;

function HumanSize(B: Int64): String;
begin
  if B >= 1073741824 then
    Result := IntToStr(B div 1073741824) + '.' +
              Frac2(B mod 1073741824, 1073741824) + ' GB'
  else if B >= 1048576 then
    Result := IntToStr(B div 1048576) + '.' +
              Frac2(B mod 1048576, 1048576) + ' MB'
  else if B >= 1024 then
    Result := IntToStr(B div 1024) + ' KB'
  else
    Result := IntToStr(B) + ' B';
end;

// 问那一句。
//
// ── 为什么是 MsgBox，而不是一个带复选框的自定义窗体 ──────────
//
// 原本照 Ollama 写了一个 TSetupForm + TNewCheckBox 的版本。两件事把它
// 否掉了：
//
// 1. `CreateCustomForm()` 在这台机器的 Inno 6 上**编译不过**
//    （「Invalid number of parameters」）。绕过去要走 `TSetupForm.Create`
//    并自己接管字体缩放与居中。
// 2. 而那条路真正的代价是**验不了**：手搓窗体在高 DPI、不同系统字体、
//    不同 Windows 版本上的样子，只能靠真的跑一遍卸载去看。一个只在
//    用户卸载时才出现一次的界面，是这个仓库里最难拿到反馈的地方。
//
// `MB_YESNO` 的行为是确定的：没有 Esc 退路（不会误触），
// `MB_DEFBUTTON2` 让默认停在「否」。而这一问要传达的四件事 ——
// **在哪、多大、里面是什么、删了回不来** —— 一个字都没少。
//
// 复选框比它好在哪：好在不用读一段话。这里换来的是「一定长这样」。
function ShouldDeleteUserData(): Boolean;
var
  Entries: Integer;
  Bytes: Int64;
  SizeText: String;
begin
  Result := False;
  if not DirExists(UserDataDir()) then
    Exit;

  Entries := 0;
  Bytes := DirSize(UserDataDir(), Entries);
  if Entries > MaxScanEntries then
    SizeText := '至少 ' + HumanSize(Bytes)
  else
    SizeText := HumanSize(Bytes);

  Result := MsgBox(
    '程序文件已经在删了。你的本地数据是单独的一份，要不要一起删？' + #13#10 + #13#10 +
    UserDataDir() + #13#10 +
    '大小：' + SizeText + #13#10 + #13#10 +
    '里面是会话与消息、附件、工作区绑定、MCP 配置、启动记录。' + #13#10 +
    '保留的话，重装之后接着用。' + #13#10 + #13#10 +
    '注意：cortex 命令行工具用的是同一个目录 —— 它还装着的话，' +
    '删掉之后它下次跑起来会重新建出来。' + #13#10 + #13#10 +
    '「是」= 删掉这些数据，删了就没了' + #13#10 +
    '「否」= 保留（默认）',
    mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  // 静默卸载**一律保留**：没有人在屏幕前回答这道题，而擅自替他删掉
  // 全部历史会话是这里唯一不可撤销的动作
  if UninstallSilent() then
    Exit;

  case CurUninstallStep of
    // 问在删文件之前：卸载走到 usPostUninstall 时用户多半已经走开了，
    // 一个没人看的弹窗等于没问
    usUninstall:
      DeleteUserData := ShouldDeleteUserData();
    usPostUninstall:
      if DeleteUserData then
        DelTree(UserDataDir(), True, True, True);
  end;
end;
