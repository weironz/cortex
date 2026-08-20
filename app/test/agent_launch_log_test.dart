import 'dart:convert';
import 'dart:io';

import 'package:cortex_app/core/agent_launch_log.dart';
import 'package:flutter_test/flutter_test.dart';

/// 启动诊断文件。
///
/// 它存在的理由是「内存里那份尾部跟着进程一起死」，所以这里测的全是
/// **跨进程还成不成立**的性质：写得下去、切得干净、坏了不许连累 agent。
void main() {
  late Directory tmp;
  late AgentLaunchLog log;

  setUp(() {
    tmp = Directory.systemTemp.createTempSync('cortex-launchlog');
    log = AgentLaunchLog(File('${tmp.path}/logs/agent-launch.jsonl'));
  });
  tearDown(() => tmp.deleteSync(recursive: true));

  List<Map<String, Object?>> read() => log.file
      .readAsLinesSync()
      .where((l) => l.trim().isNotEmpty)
      .map((l) => jsonDecode(l) as Map<String, Object?>)
      .toList();

  test('目录不存在时自己建出来', () {
    // agent 第一次启动就崩的话，`logs/` 还没人建过 —— 那恰恰是最需要
    // 这份记录的一次。要求目录先存在，等于在最重要的场景下什么都不记
    expect(log.file.parent.existsSync(), isFalse);
    log.spawn(exe: 'a.exe', pid: 1, remote: 'https://x/api', llm: 'proxy');
    expect(read().single['ev'], 'spawn');
  });

  test('每条都带时间戳，且是追加不是覆盖', () {
    log.spawn(exe: 'a', pid: 1, remote: 'r', llm: 'proxy');
    log.ready(origin: 'http://127.0.0.1:7499', ms: 800);
    log.exited(code: 0, expected: true);

    final rows = read();
    expect(
      rows.map((r) => r['ev']),
      ['spawn', 'ready', 'exit'],
      reason:
          '崩溃循环的形状只有在多次启动的记录排在一起时才看得出来，'
          '每次覆盖就只剩最后一条',
    );
    for (final r in rows) {
      expect(
        DateTime.tryParse(r['t']! as String),
        isNotNull,
        reason: '用户把文件发过来时，「什么时候」不能靠猜',
      );
    }
  });

  test('stderr 尾部只留最后几行，且超长的一行会截断', () {
    final many = List.generate(40, (i) => 'line$i').join('\n');
    log.exited(code: 101, expected: false, tail: '$many\n${'x' * 900}');

    final tail = (read().single['tail']! as List).cast<String>();
    expect(tail.length, lessThanOrEqualTo(12));
    expect(tail.first, isNot('line0'), reason: '留的是最后几行 —— 死因在末尾，不在开头');
    expect(
      tail.last.length,
      lessThanOrEqualTo(401),
      reason: 'panic backtrace 单行可以很长；一条日志不该比它要诊断的问题还难读',
    );
  });

  test('没有 tail 时不写这个字段，而不是写空数组', () {
    log.exited(code: 0, expected: true);
    expect(
      read().single.containsKey('tail'),
      isFalse,
      reason: '空数组和「它什么都没说」在读的人眼里是两回事',
    );
  });

  test('超上限就砍前半截，且切在行边界上', () {
    // 造一条足够胖的记录，几十次就能顶到上限。
    // 必须是**多行**：单行会被截到 400 字符，一条记录还不到半 KB
    final fat = List.generate(12, (_) => 'y' * 390).join('\n');
    var wrote = 0;
    while (log.file.existsSync() == false ||
        log.file.lengthSync() <= AgentLaunchLog.maxBytes) {
      log.failed(why: '第 $wrote 次', tail: fat);
      wrote++;
      expect(wrote, lessThan(500), reason: '没在涨，说明写失败被吞了');
    }
    log.failed(why: '压过上限那一次', tail: fat);

    expect(
      log.file.lengthSync(),
      lessThan(AgentLaunchLog.maxBytes + 8192),
      reason:
          '崩溃循环每 6 秒写一条 —— 不封顶的话一夜几十 MB，'
          '一个诊断文件把用户磁盘吃掉，比没有这个文件更糟',
    );
    // 切在行边界：整份还能一行行解析出来
    expect(
      () => read(),
      returnsNormally,
      reason:
          '不从换行处切的话，第一行会是半条 JSON，'
          '而读的人（和 doctor）要为这个自找的问题写一段容错',
    );
    expect(read().last['why'], '压过上限那一次');
  });

  test('写不进去也不抛 —— 诊断设施不许把被诊断的东西弄挂', () {
    // 用一个「父目录是文件」的路径：createSync 必失败
    final blocker = File('${tmp.path}/blocked')..writeAsStringSync('x');
    final broken = AgentLaunchLog(File('${blocker.path}/logs/a.jsonl'));
    expect(
      () => broken.spawn(exe: 'a', pid: 1, remote: 'r', llm: 'proxy'),
      returnsNormally,
      reason: '磁盘满、目录被杀毒软件锁住，都不该让 agent 起不来',
    );
  });
}
