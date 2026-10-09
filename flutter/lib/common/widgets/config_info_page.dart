import 'dart:async';
import 'dart:convert';

import 'package:desktop_multi_window/desktop_multi_window.dart'
    show WindowController;
import 'package:flutter/material.dart';
import 'package:flutter_hbb/common.dart';
import 'package:flutter_hbb/main.dart' show kWindowId;
import 'package:flutter_hbb/utils/multi_window_manager.dart' show WindowType;
import 'package:get/get.dart';
import 'package:provider/provider.dart';

import '../../models/model.dart';
import '../../models/platform_model.dart';

/// Z远程协助(zremote66): holds the parsed config-info payload for the current
/// isolate's session. The peer pushes it back through a MessageBox whose msgtype
/// is "zremote66-config-info"; `msgBox()` in common.dart forwards the text here.
class ConfigInfoController extends ChangeNotifier {
  static final ConfigInfoController instance = ConfigInfoController._();
  ConfigInfoController._();

  Map<String, dynamic>? data;
  bool loading = true;
  String? error;

  // Z远程协助: 密码错误(re-input-password)标志。为 true 时页面应弹出密码重输框，
  // 用户输入新密码后重连；而不是只在页面内显示"密码错误"而无法重试。
  bool passwordError = false;

  // Z远程协助: 软件卸载 / 服务启停 / refresh 操作的状态与结果回传。
  bool opPending = false;
  String? opResultMessage;

  void update(String raw) {
    // Empty payload (collection failed on the peer) -> every tab falls back to
    // "暂不支持" instead of surfacing a parse error.
    _resetOpState();
    if (raw.trim().isEmpty) {
      data = {};
      error = null;
      loading = false;
      notifyListeners();
      return;
    }
    try {
      final decoded = jsonDecode(raw);
      if (decoded is Map<String, dynamic>) {
        data = decoded;
        error = null;
      } else {
        error = '数据格式错误';
      }
    } catch (e) {
      error = '解析失败: $e';
    }
    loading = false;
    notifyListeners();
  }

  void reset() {
    data = null;
    loading = true;
    error = null;
    _resetOpState();
    notifyListeners();
  }

  // Z远程协助: connection-level failure surfaced from the Rust side (peer
  // offline, wrong password, permission denied, login rejected, ...). This isolate
  // hosts ONLY the config page, so such events are routed here instead of a
  // blocking dialog: stop the spinner and show the reason in-page. Never throws.
  void onConnectionError(String? title, String? text) {
    loading = false;
    data ??= {};
    final t = (text ?? '').trim();
    final ti = (title ?? '').trim();
    error = t.isNotEmpty
        ? t
        : (ti.isNotEmpty
            ? ti
            : '连接失败，请确认被控端在线且密码正确');
    notifyListeners();
  }

  // Z远程协助: 密码错误。置 passwordError 标志，通知页面弹出密码重输界面。
  void onPasswordError() {
    loading = false;
    passwordError = true;
    notifyListeners();
  }

  // Z远程协助: 页面已弹出密码重输框后清除标志，避免同一错误重复触发。
  void clearPasswordError() {
    if (passwordError) {
      passwordError = false;
      notifyListeners();
    }
  }

  void _resetOpState() {
    opPending = false;
    opResultMessage = null;
  }

  // Z远程协助: 向被控端发送配置操作（uninstall / service_stop / service_start /
  // refresh）。bind.sessionSendConfigOp 由 CI bridge 重新生成后可用。
  Future<void> sendOp(Map<String, dynamic> payload) async {
    opPending = true;
    notifyListeners();
    try {
      await bind.sessionSendConfigOp(
          sessionId: gFFI.sessionId, json: jsonEncode(payload));
    } catch (e) {
      opPending = false;
      opResultMessage = '发送失败: $e';
      notifyListeners();
    }
  }

  // Z远程协助: 被控端回传 {ok, message, data?}。
  // - ok 且 data 非空 => refresh 场景，data 为完整配置 JSON，直接更新全部 Tab。
  // - ok 且无 data => 操作执行成功，自动发 refresh 拉最新列表。
  // - ok:false => 面板展示 message（含"权限"时由 UI 高亮提示）。
  void onOpResult(String raw) {
    opPending = false;
    try {
      final decoded = jsonDecode(raw);
      if (decoded is! Map) {
        opResultMessage = '返回数据格式错误';
        notifyListeners();
        return;
      }
      final ok = decoded['ok'] == true;
      final msg = decoded['message']?.toString() ?? '';
      if (ok && decoded['data'] != null) {
        // refresh 回包：整体重建数据（分页 State 内部已 clamp 页码）。
        update(jsonEncode(decoded['data']));
        opResultMessage = msg.isEmpty ? '刷新成功' : msg;
      } else if (ok) {
        opResultMessage = msg.isEmpty ? '操作成功' : msg;
        notifyListeners();
        // 成功后自动刷新列表。
        sendOp({'op': 'refresh'});
        return;
      } else {
        opResultMessage = msg.isEmpty ? '操作失败' : msg;
      }
    } catch (e) {
      opResultMessage = '解析失败: $e';
    }
    notifyListeners();
  }
}

// Z远程协助: 自定义标题栏高度（桌面子窗口自绘头部，替代系统 AppBar）。
const double _kConfigTitleBarHeight = 42;

// Z远程协助: 分页大小（已安装软件 / 服务列表）。
const int _kConfigPageSize = 100;

// Z远程协助: GB 数值统一保留 1 位小数。Rust 侧已圆整，这里再做防御性
// 格式化，兼容旧被控端发来的多位小数 / int / 字符串 / 非法值。
String _fmtGb(dynamic v) {
  if (v is num) return v.toStringAsFixed(1);
  if (v is String) {
    final d = double.tryParse(v.trim());
    if (d != null) return d.toStringAsFixed(1);
  }
  return '0.0';
}

// Z远程协助: 内存自适应单位——输入为 MB，≥1024 自动转 GB，否则保留 MB。
String _fmtMem(double mb) {
  if (mb >= 1024) return '${(mb / 1024).toStringAsFixed(1)}GB';
  return '${mb.toStringAsFixed(1)}MB';
}

// Z远程协助: 把任意取值安全转成 double（用于容量/速率等数值判定）。
double _toGb(dynamic v) {
  if (v is num) return v.toDouble();
  if (v is String) return double.tryParse(v.trim()) ?? 0.0;
  return 0.0;
}

// Z远程协助: 把任意取值安全转成 int（核心数/线程数）。
int _toInt(dynamic v) {
  if (v is num) return v.toInt();
  if (v is String) {
    return int.tryParse(v.trim()) ?? double.tryParse(v.trim())?.toInt() ?? 0;
  }
  return 0;
}

// Z远程协助: 淡蓝色圆角行号标签（替代纯文字行号）。
Widget _numBadge(int index) {
  return Container(
    width: 30,
    height: 22,
    alignment: Alignment.center,
    decoration: BoxDecoration(
      color: const Color(0xFFE3F2FD),
      borderRadius: BorderRadius.circular(4),
    ),
    child: Text(
      '${index + 1}',
      style: const TextStyle(
        color: Color(0xFF1565C0),
        fontSize: 12,
        fontWeight: FontWeight.bold,
      ),
    ),
  );
}

// Z远程协助: 硬件配置 Tab 的一行（label=标签列，value=值列，group=大类分组，
// 用于分隔线：同组淡线、不同大类中灰线）。
class _HwRow {
  final String label;
  final String value;
  final int group;
  // Z远程协助: 值列尾部的可点击操作链接(如"重启")。
  final String actionLabel;
  final VoidCallback? onAction;
  const _HwRow({
    required this.label,
    required this.value,
    required this.group,
    this.actionLabel = '',
    this.onAction,
  });
}

/// Full-screen (mobile) / embedded (desktop) tabbed view of the peer's
/// hardware/software configuration.
class ConfigInfoPage extends StatefulWidget {
  final String id;
  final String? password;
  final bool? isSharedPassword;
  final bool? forceRelay;
  // Z远程协助: true 表示主控端已持有该 peer 的认证会话（密码已通过），
  // 本页不发起新的 LoginRequest，仅等待主窗口转发被控端回传的配置数据。
  final bool waitForData;
  // Z远程协助: 子窗口原始参数 map。本机「查看本机配置」用它读取 local/text
  // 直达本机 JSON，随窗口参数立即渲染，避免广播竞态导致的白屏/一直转圈。
  final Map<String, dynamic>? params;
  // Z远程协助: 主窗口内模态层模式(桌面端改用主窗口全屏 Dialog 展示)。关闭走
  // Navigator.pop(纯本地、任何情况都能关)，数据/密码错误/连接错误都在主 isolate
  // 闭环，不依赖跨窗口 channel——根治"密码输错/连接问题子窗口关不掉卡死"。
  final bool dialogMode;

  const ConfigInfoPage({
    Key? key,
    required this.id,
    this.password,
    this.isSharedPassword,
    this.forceRelay,
    this.waitForData = false,
    this.params,
    this.dialogMode = false,
  }) : super(key: key);

  @override
  State<ConfigInfoPage> createState() => _ConfigInfoPageState();
}

class _ConfigInfoPageState extends State<ConfigInfoPage> {
  // Z远程协助: dialogMode(主窗口模态层)或桌面子窗口(非 macOS)使用自绘 42px
  // 标题栏 + 关闭按钮；移动端/其它情况保留默认 AppBar。
  bool get _useCustomTitleBar => widget.dialogMode || (isDesktop && !isMacOS);
  // Z远程协助: 本机「查看本机配置」标题用「本机配置」，远程用「被控端配置信息」。
  bool get _isLocalConfig => widget.params?['local'] == true;
  String get _windowTitle => _isLocalConfig ? '本机配置' : '被控端配置信息';
  // Z远程协助: 防抖——避免同一密码错误在弹框关闭/重建时重复触发重输框。
  bool _pwDialogShown = false;
  // Z远程协助: 连接获取超时器——发起连接后 30s 内既无数据也无错误回调时，
  // 自动停止转圈并显示可关闭的错误提示，避免"连接不上/获取不到数据"时白屏且无法关闭。
  Timer? _fetchTimeout;

  @override
  void initState() {
    super.initState();
    ConfigInfoController.instance.reset();
    ConfigInfoController.instance.addListener(_onConfigControllerChanged);
    // Z远程协助: 本机「查看本机配置」——数据已随窗口参数直达，直接渲染，不发起连接，
    // 彻底规避子窗口 MessageHandler 未就绪时广播丢失导致的白屏/一直转圈。
    final localText = widget.params?['text']?.toString();
    if (localText != null && localText.isNotEmpty) {
      ConfigInfoController.instance.update(localText);
    } else if (!widget.waitForData) {
      gFFI.ffiModel.updateEventListener(gFFI.sessionId, widget.id);
      gFFI.start(
        widget.id,
        isConfigInfo: true,
        password: widget.password,
        isSharedPassword: widget.isSharedPassword,
        forceRelay: widget.forceRelay,
      );
      _armFetchTimeout();
    }
    // Z远程协助: 删除全屏模态 showLoading——它会盖住自绘标题栏关闭按钮，
    // 连接卡住（密码验证/离线重试）时用户点不到关闭、窗口关不掉。
    // loading 状态由 build 里的 Consumer(c.loading) 展示，窗口任何时刻可交互。
  }

  // Z远程协助: 30s 无结果即超时置错(仍可关闭)。waitForData/本机配置无独立连接，不设超时。
  void _armFetchTimeout() {
    _fetchTimeout?.cancel();
    _fetchTimeout = Timer(const Duration(seconds: 30), () {
      if (!mounted) return;
      final c = ConfigInfoController.instance;
      if (c.loading && !c.passwordError) {
        c.onConnectionError(null, '获取配置信息超时，请确认被控端在线且密码正确');
      }
    });
  }

  @override
  void dispose() {
    _fetchTimeout?.cancel();
    ConfigInfoController.instance.removeListener(_onConfigControllerChanged);
    // Z远程协助: 非复用模式下本页自己发起了会话，释放之；复用模式下会话属于主窗口
    // isolate，本页只读复用，不能 close。
    if (!widget.waitForData) {
      gFFI.close();
    }
    super.dispose();
  }

  // Z远程协助: 密码错误标志置位时，弹出密码重输框保持输入界面，可再次输入别的密码重连。
  void _onConfigControllerChanged() {
    final c = ConfigInfoController.instance;
    if (!c.passwordError || _pwDialogShown || !mounted) return;
    _pwDialogShown = true;
    WidgetsBinding.instance.addPostFrameCallback((_) async {
      c.clearPasswordError();
      final pw = await _promptRetryPassword();
      _pwDialogShown = false;
      if (pw != null && pw.isNotEmpty) {
        await _retryWithPassword(pw);
      }
    });
  }

  // Z远程协助: 密码重输框。不预填；提示上次密码错误，可再次输入别的密码。
  Future<String?> _promptRetryPassword() async {
    final controller = TextEditingController();
    try {
      if (!mounted) return null;
      return await showDialog<String>(
        context: context,
        builder: (ctx) => AlertDialog(
          title: const Text('密码错误'),
          content: SizedBox(
            width: 360,
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                const Text('密码不正确，请重新输入被控端密码：'),
                const SizedBox(height: 10),
                TextField(
                  controller: controller,
                  obscureText: true,
                  autofocus: true,
                  decoration: const InputDecoration(
                    labelText: '密码',
                    border: OutlineInputBorder(),
                  ),
                ),
              ],
            ),
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(ctx),
              child: const Text('取消'),
            ),
            TextButton(
              onPressed: () => Navigator.pop(ctx, controller.text),
              child: const Text('重新连接'),
            ),
          ],
        ),
      );
    } finally {
      controller.dispose();
    }
  }

  // Z远程协助: 用新密码重新发起配置信息连接。先释放旧会话再 start。
  // 注意: gFFI.close() 是异步(内部 await bind.sessionClose)，必须 await，
  // 否则旧会话未真正关闭就 start 重建同一 sessionId，rust 端会话冲突导致连接
  // 建立失败、前端一直转圈(修复"重输密码后一直加载")。
  Future<void> _retryWithPassword(String pw) async {
    if (!mounted) return;
    try {
      await gFFI.close();
    } catch (_) {}
    ConfigInfoController.instance.reset();
    _armFetchTimeout();
    gFFI.ffiModel.updateEventListener(gFFI.sessionId, widget.id);
    gFFI.start(
      widget.id,
      isConfigInfo: true,
      password: pw,
      isSharedPassword: widget.isSharedPassword,
      forceRelay: widget.forceRelay,
    );
  }

  // Z远程协助: 运行时间行「重启」链接——二次确认后向受控端发送 RestartRemoteDevice。
  // 被控端按自身平台执行重启(win force_reboot / linux reboot / mac reboot)。
  Future<void> _confirmReboot() async {
    if (!mounted || _isLocalConfig) return;
    final ok = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('确认重启'),
        content: const Text('是否确认重启受控端？'),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx, false),
            child: const Text('取消'),
          ),
          TextButton(
            onPressed: () => Navigator.pop(ctx, true),
            child: const Text('确认'),
          ),
        ],
      ),
    );
    if (ok != true || !mounted) return;
    try {
      bind.sessionRestartRemoteDevice(sessionId: gFFI.sessionId);
    } catch (e) {
      debugPrint('send restart remote device failed: $e');
    }
  }

  // Z远程协助: 主窗口模态层关闭——纯本地 Navigator.pop，任何状态(loading/error/data)
  // 都能立即关闭，不依赖任何跨窗口 channel。会话释放由 dispose() 统一处理。
  void _onCloseDialogMode() {
    if (mounted) Navigator.of(context).pop();
  }

  // Z远程协助: 关闭按钮逻辑健壮化——任一步失败都不阻断后续步骤，保证窗口最终能关掉；
  // kWindowId 为 null 时降级直接释放本页会话。
  Future<void> _onCloseDesktopWindow() async {
    if (kWindowId == null) {
      gFFI.close();
      return;
    }
    try {
      await saveWindowPosition(WindowType.ConfigInfo, windowId: kWindowId);
    } catch (_) {
      // 保存窗口位置失败可容忍，不影响关闭流程。
    }
    try {
      await WindowController.fromWindowId(kWindowId!).setPreventClose(false);
    } catch (_) {}
    try {
      await WindowController.fromWindowId(kWindowId!).close();
    } catch (_) {}
  }

  // Z远程协助: 自绘头部，固定 42px，左侧标题、右侧关闭。
  Widget _buildCustomTitleBar() {
    return Container(
      height: _kConfigTitleBarHeight,
      color: Theme.of(context).primaryColor,
      child: Row(
        children: [
          const SizedBox(width: 12),
          Expanded(
            child: Text(
              _windowTitle,
              style: TextStyle(color: Colors.white, fontSize: 14),
            ),
          ),
          IconButton(
            icon: const Icon(Icons.close, size: 18, color: Colors.white),
            tooltip: '关闭',
            onPressed:
                widget.dialogMode ? _onCloseDialogMode : _onCloseDesktopWindow,
          ),
          const SizedBox(width: 4),
        ],
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final customTitleBar = _useCustomTitleBar;
    return Scaffold(
      appBar: customTitleBar
          ? null
          : AppBar(
              title: Text(_windowTitle),
            ),
      // Z远程协助: 自绘标题栏(含关闭按钮)常驻在最外层，loading/error/data 任何
      // 状态下都渲染，确保连接不上/获取不到数据时弹层仍可正常关闭(不再白屏无法关)。
      body: Column(
        children: [
          if (customTitleBar) _buildCustomTitleBar(),
          Expanded(
            child: ChangeNotifierProvider.value(
              value: ConfigInfoController.instance,
              child: Consumer<ConfigInfoController>(
                builder: (context, c, _) {
                  if (c.loading) {
                    return const Center(child: CircularProgressIndicator());
                  }
                  if (c.error != null) {
                    return Center(
                      child: Column(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          const Icon(Icons.error_outline,
                              color: Colors.redAccent, size: 40),
                          const SizedBox(height: 12),
                          Padding(
                            padding:
                                const EdgeInsets.symmetric(horizontal: 24),
                            child: Text(
                              c.error!,
                              textAlign: TextAlign.center,
                            ),
                          ),
                          const SizedBox(height: 16),
                          OutlinedButton(
                            onPressed: widget.dialogMode
                                ? _onCloseDialogMode
                                : _onCloseDesktopWindow,
                            child: const Text('关闭'),
                          ),
                        ],
                      ),
                    );
                  }
                  if (c.data == null) {
                    return const Center(child: Text('暂无数据'));
                  }
                  return _buildTabs(context, c.data!);
                },
              ),
            ),
          ),
        ],
      ),
    );
  }

  Widget _buildTabs(BuildContext context, Map<String, dynamic> data) {
    return DefaultTabController(
      length: 4,
      child: Column(
        children: [
          const TabBar(
            labelColor: Colors.blue,
            tabs: [
              Tab(text: '硬件配置'),
              Tab(text: '用户列表'),
              Tab(text: '已安装软件'),
              Tab(text: '服务列表'),
            ],
          ),
          Expanded(
            child: TabBarView(
              children: [
                _hardwareTab(data),
                _ListTabView(
                  rows: _userRows(data['users']),
                  columns: const ['名称', '全名', '管理员'],
                  emptyText: '暂不支持',
                  kind: 'user',
                ),
                _ListTabView(
                  rows: _rows(data['software'], ['name', 'version', 'publisher']),
                  columns: const ['名称', '版本', '发布者'],
                  emptyText: '暂不支持',
                  paginated: true,
                  searchable: true,
                  kind: 'software',
                ),
                _ListTabView(
                  rows: _rows(data['services'], ['name', 'status', 'start_type']),
                  columns: const ['名称', '状态', '启动类型'],
                  emptyText: '暂不支持',
                  paginated: true,
                  searchable: true,
                  kind: 'service',
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }

  List<List<String>> _rows(dynamic raw, List<String> keys) {
    final List<List<String>> out = [];
    if (raw is List) {
      for (final item in raw) {
        if (item is Map) {
          out.add(keys.map((k) => (item[k] ?? '').toString()).toList());
        }
      }
    }
    return out;
  }

  // Users rows with the admin flag rendered as 是/否.
  List<List<String>> _userRows(dynamic raw) {
    final List<List<String>> out = [];
    if (raw is List) {
      for (final item in raw) {
        if (item is Map) {
          final admin = item['admin'] == true ? '是' : '否';
          out.add([
            (item['name'] ?? '').toString(),
            (item['full_name'] ?? '').toString(),
            admin,
          ]);
        }
      }
    }
    return out;
  }

  // Z远程协助: 硬件配置 Tab。固定显示顺序：
  // 操作系统 -> 厂家型号 -> 主板 -> CPU -> 内存 -> 硬盘 -> 显卡 -> 分辨率 -> 网卡。
  // 任一字段取不到（空串 / 0 / 缺失）时整行隐藏，不显示"未知"。
  Widget _hardwareTab(Map<String, dynamic> data) {
    // 防御: 任一字段类型不符（非 Map）时按空 Map 处理, 绝不上抛导致红屏。
    Map hwMap(dynamic v) => v is Map ? v : const {};
    final os = hwMap(data['os']);
    final machine = hwMap(data['machine']);
    final board = hwMap(data['board']);
    final cpu = hwMap(data['cpu']);
    final mem = hwMap(data['memory']);
    final disk = hwMap(data['disk']);

    final rows = <_HwRow>[];

    // group 0: 操作系统
    final osParts = [
      os['name']?.toString() ?? '',
      os['version']?.toString() ?? '',
      os['arch']?.toString() ?? '',
    ].where((s) => s.isNotEmpty).join(' ');
    if (osParts.isNotEmpty) {
      rows.add(_HwRow(label: '操作系统', value: osParts, group: 0));
    }

    // 计算机名(操作系统下方)
    {
      final hn = data['hostname']?.toString() ?? '';
      if (hn.isNotEmpty) {
        rows.add(_HwRow(label: '计算机名', value: hn, group: 0));
      }
    }
    // 运行时间(计算机名下方): 开机时间 + 已运行分钟(已运行前 2 个空格)
    {
      final bt = data['boot_time'];
      final tm = data['total_minutes'];
      final parts = <String>[];
      if (bt is num && bt.toInt() > 0) {
        final dt = DateTime.fromMillisecondsSinceEpoch(bt.toInt() * 1000);
        String two(int n) => n.toString().padLeft(2, '0');
        parts.add('开机时间：${dt.year}-${two(dt.month)}-${two(dt.day)} '
            '${two(dt.hour)}:${two(dt.minute)}:${two(dt.second)}');
      }
      if (tm is num) {
        final m = tm.toInt();
        if (m < 60) {
          parts.add('  已运行：$m分钟');
        } else if (m < 1440) {
          final h = m ~/ 60;
          final r = m % 60;
          parts.add('  已运行：$h小时${r > 0 ? '$r分钟' : ''}');
        } else {
          final d = m ~/ 1440;
          final rem = m % 1440;
          final h = rem ~/ 60;
          final r = rem % 60;
          final sb = StringBuffer('  已运行：$d天');
          if (h > 0) sb.write('$h小时');
          if (r > 0) sb.write('$r分钟');
          parts.add(sb.toString());
        }
      }
      if (parts.isNotEmpty) {
        rows.add(_HwRow(
          label: '运行时间',
          value: parts.join(''),
          group: 0,
          // Z远程协助: 本机「查看本机配置」不显示重启链接；远程受控端显示可点击
          // 「重启」，二次确认后向受控端发送 RestartRemoteDevice。
          actionLabel: _isLocalConfig ? '' : '重启',
          onAction: _isLocalConfig ? null : _confirmReboot,
        ));
      }
    }
    // IP地址: 本地IP / 互联网IP(操作系统下方)
    {
      final li = data['local_ip']?.toString() ?? '';
      final pi = data['public_ip']?.toString() ?? '';
      final ipParts = <String>[];
      if (li.isNotEmpty) ipParts.add('本地IP:$li');
      if (pi.isNotEmpty && pi != li) ipParts.add('互联网IP:$pi');
      if (ipParts.isNotEmpty) {
        rows.add(_HwRow(label: 'IP地址', value: ipParts.join('  '), group: 0));
      }
    }

    // group 1: 厂家型号（machine.vendor / machine.model）
    {
      final v = machine['vendor']?.toString() ?? '';
      final m = machine['model']?.toString() ?? '';
      final value = [v, m].where((s) => s.isNotEmpty).join(' / ');
      if (value.isNotEmpty) {
        rows.add(_HwRow(label: '厂家型号', value: value, group: 1));
      }
    }

    // group 2: 主板（厂商 / 型号，serial 非空追加序列号）
    {
      final vendor = board['vendor']?.toString() ?? '';
      final model = board['model']?.toString() ?? '';
      final serial = board['serial']?.toString() ?? '';
      if (vendor.isNotEmpty || model.isNotEmpty || serial.isNotEmpty) {
        var value = [vendor, model].where((s) => s.isNotEmpty).join(' / ');
        if (serial.isNotEmpty) value += ' 序列号:$serial';
        rows.add(_HwRow(label: '主板', value: value, group: 2));
      }
    }

    // group 3: CPU（cores 为 0 时只显示线程数；freq_mhz 为 0 时不显示主频）
    {
      final model = cpu['model']?.toString() ?? '';
      final cores = _toInt(cpu['cores']);
      final threads = _toInt(cpu['threads']);
      final freq = _toGb(cpu['freq_mhz']);
      if (model.isNotEmpty || cores > 0 || threads > 0 || freq > 0) {
        final detail = <String>[];
        if (cores > 0) detail.add('核心:$cores');
        detail.add('线程:$threads');
        if (freq > 0) detail.add('主频:${freq.toStringAsFixed(0)}MHz');
        final value = [
          model,
          detail.join(' '),
        ].where((s) => s.isNotEmpty).join(' / ');
        rows.add(_HwRow(label: 'CPU', value: value, group: 3));
      }
    }

    // group 4: 内存（总/剩余必显示，已用>0 才显示，brand 非空追加品牌）
    // 单位：后端输出 total_mb/available_mb/used_mb（MB），前端 ≥1024 自动转 GB；兼容旧格式 total_gb。
    {
      final totalMb = mem['total_mb'] != null ? _toGb(mem['total_mb']) : null;
      final usedMb = mem['used_mb'] != null ? _toGb(mem['used_mb']) : null;
      final availMb = mem['available_mb'] != null ? _toGb(mem['available_mb']) : null;
      final brand = mem['brand']?.toString() ?? '';
      if (totalMb != null || usedMb != null || availMb != null) {
        final parts = <String>[];
        if (totalMb != null && totalMb > 0) parts.add('总:${_fmtMem(totalMb)}');
        if (availMb != null && availMb > 0) parts.add('剩余:${_fmtMem(availMb)}');
        if (usedMb != null && usedMb > 0) parts.add('已用:${_fmtMem(usedMb)}');
        var value = parts.join(' ');
        if (brand.isNotEmpty) value += ' (品牌:$brand)';
        rows.add(_HwRow(label: '内存', value: value, group: 4));
      } else {
        // 旧被控端格式（total_gb 等），按 GB 显示。
        final total = _toGb(mem['total_gb']);
        final used = _toGb(mem['used_gb']);
        final avail = _toGb(mem['available_gb']);
        if (total > 0 || used > 0 || avail > 0 || brand.isNotEmpty) {
          final parts = <String>[];
          if (total > 0) parts.add('总:${_fmtGb(total)}GB');
          if (avail > 0) parts.add('剩余:${_fmtGb(avail)}GB');
          if (used > 0) parts.add('已用:${_fmtGb(used)}GB');
          var value = parts.join(' ');
          if (brand.isNotEmpty) value += ' (品牌:$brand)';
          rows.add(_HwRow(label: '内存', value: value, group: 4));
        }
      }
    }

    // group 5: 硬盘（每块物理磁盘一行，编号 1. 2. ；取消分区子行）
    {
      final disks = disk['disks'];
      if (disks is List && disks.isNotEmpty) {
        final multiDisk = disks.length > 1;
        var dIdx = 0;
        for (final d in disks) {
          if (d is! Map) continue;
          final dName = d['name']?.toString() ?? '';
          final dModel = d['model']?.toString() ?? '';
          final dTotal = _toGb(d['total_gb']);
          final dFree = _toGb(d['free_gb']);
          final head = <String>[];
          if (dName.isNotEmpty) head.add(dName);
          if (dModel.isNotEmpty) head.add('型号:$dModel');
          final headStr = head.length > 1 ? '${head[0]} (${head[1]})' : head.join();
          final segs = <String>[];
          if (headStr.isNotEmpty) segs.add(headStr);
          if (dTotal > 0) segs.add('总:${_fmtGb(dTotal)}GB');
          if (dFree > 0) segs.add('剩余:${_fmtGb(dFree)}GB');
          if (segs.isEmpty) continue;
          var value = segs.join(' ');
          if (multiDisk) value = '${dIdx + 1}.  $value';
          rows.add(_HwRow(label: '硬盘', value: value, group: 5));
          dIdx++;
        }
      } else {
        // 旧格式回退：仅顶层汇总行（不再平铺分区）。
        final total = _toGb(disk['total_gb']);
        final used = _toGb(disk['used_gb']);
        final free = _toGb(disk['free_gb']);
        final summary = <String>[];
        if (total > 0) summary.add('总:${_fmtGb(total)}GB');
        if (used > 0) summary.add('已用:${_fmtGb(used)}GB');
        if (free > 0) summary.add('剩余:${_fmtGb(free)}GB');
        if (summary.isNotEmpty) {
          rows.add(_HwRow(label: '硬盘', value: summary.join(' '), group: 5));
        }
      }
    }

    // group 6: 显卡（字符串非空才显示）
    final gpu = data['gpu']?.toString() ?? '';
    if (gpu.isNotEmpty) rows.add(_HwRow(label: '显卡', value: gpu, group: 6));

    // group 7: 分辨率。screen 键存在但值为空串(如无显示器的 headless/服务会话)时
    // 显示"暂不支持"; screen 键缺失(旧被控端)则整行隐藏, 保持兼容。
    if (data.containsKey('screen')) {
      final screen = data['screen']?.toString() ?? '';
      rows.add(_HwRow(
          label: '分辨率',
          value: screen.isEmpty ? '暂不支持' : screen,
          group: 7));
    }

    // group 8: 网卡（data.net[]；缺失/空数组则整组不显示）
    {
      final nets = data['net'];
      if (nets is List && nets.isNotEmpty) {
        final multi = nets.length > 1;
        var nIdx = 0;
        for (final n in nets) {
          if (n is! Map) continue;
          final name = n['name']?.toString() ?? '';
          final mac = n['mac']?.toString() ?? '';
          final ip = n['ip']?.toString() ?? '';
          final rx = _toGb(n['rx_kbps']);
          final tx = _toGb(n['tx_kbps']);
          final left = <String>[];
          if (name.isNotEmpty) left.add(name);
          if (mac.isNotEmpty) left.add('($mac)');
          if (ip.isNotEmpty) left.add(ip);
          var value = left.join(' ');
          final speed = <String>[];
          if (tx > 0) speed.add('上传:${tx.toStringAsFixed(0)} KB/s');
          if (rx > 0) speed.add('下载:${rx.toStringAsFixed(0)} KB/s');
          if (speed.isNotEmpty) value += ' — ${speed.join(' ')}';
          if (value.isEmpty) continue;
          if (multi) value = '${nIdx + 1}.  $value';
          rows.add(_HwRow(label: '网卡', value: value, group: 8));
          nIdx++;
        }
      }
    }

    // 空 JSON（data 为 {}）时整体显示"暂不支持"。
    if (rows.isEmpty) {
      return const Center(child: Text('暂不支持'));
    }
    return ListView.separated(
      padding: const EdgeInsets.symmetric(vertical: 8, horizontal: 12),
      itemCount: rows.length,
      separatorBuilder: (context, i) {
        // 同组子行淡线；不同大类中灰线。
        if (rows[i].group == rows[i + 1].group) {
          return const Divider(height: 1, color: Color(0xFFF0F0F0));
        }
        return const Divider(height: 1, thickness: 1, color: Color(0xFFE0E0E0));
      },
      itemBuilder: (context, i) {
        final e = rows[i];
        return ListTile(
          dense: true,
          contentPadding:
              const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
          leading: _numBadge(i),
          title: Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              SizedBox(
                // Z远程协助: 标签列收窄到容纳约6个字，值列(Expanded)随之加宽。
                width: 88,
                child: Text(
                  e.label,
                  style: const TextStyle(
                    fontWeight: FontWeight.bold,
                    color: Color(0xFF1565C0),
                  ),
                ),
              ),
              Expanded(
                child: Row(
                  children: [
                    Flexible(child: Text(e.value)),
                    // Z远程协助: 运行时间行尾可点击操作链接(重启)，前留 2 空格。
                    if (e.actionLabel.isNotEmpty && e.onAction != null)
                      InkWell(
                        onTap: e.onAction,
                        child: Padding(
                          padding: const EdgeInsets.only(left: 8),
                          child: Text(
                            '  ${e.actionLabel}',
                            style: const TextStyle(
                              color: Color(0xFF1565C0),
                              decoration: TextDecoration.underline,
                              fontWeight: FontWeight.w600,
                            ),
                          ),
                        ),
                      ),
                  ],
                ),
              ),
            ],
          ),
        );
      },
    );
  }
}

// Z远程协助: 用户/软件/服务 列表 Tab。paginated=true 时按 _kConfigPageSize
// 分页，行号用全局序号（page*100+i+1），第 2 页第一行为 101；a)/b)/c)
// 子项编号在分页 Tab 中每页从 a) 重置，用户 Tab 连续编号。searchable=true 时
// 顶部显示搜索框，过滤后再分页；kind=software/service 行支持长按弹出操作面板。
class _ListTabView extends StatefulWidget {
  final List<List<String>> rows;
  final List<String> columns;
  final String emptyText;
  final bool paginated;
  final bool searchable;
  final String kind; // 'user' | 'software' | 'service'

  const _ListTabView({
    required this.rows,
    required this.columns,
    required this.emptyText,
    required this.kind,
    this.paginated = false,
    this.searchable = false,
  });

  @override
  State<_ListTabView> createState() => _ListTabViewState();
}

class _ListTabViewState extends State<_ListTabView> {
  int _page = 0;
  String _keyword = '';

  @override
  void didUpdateWidget(covariant _ListTabView oldWidget) {
    super.didUpdateWidget(oldWidget);
    // 数据刷新后把当前页收敛到合法范围内。
    final maxPage = (widget.rows.length / _kConfigPageSize).ceil() - 1;
    if (maxPage < 0) {
      _page = 0;
    } else if (_page > maxPage) {
      _page = maxPage;
    }
  }

  // Z远程协助: 按关键字过滤（名称/版本/发布者 或 名称/状态/启动类型 子串，
  // 大小写不敏感）。
  List<List<String>> get _filtered {
    final kw = _keyword.trim().toLowerCase();
    if (kw.isEmpty) return widget.rows;
    return widget.rows
        .where((cells) => cells.any((c) => c.toLowerCase().contains(kw)))
        .toList();
  }

  Widget _buildRow(List<String> cells, int globalIndex, int letterIndex) {
    var title = cells.isEmpty
        ? ''
        : cells.asMap().entries.map((e) {
            final col =
                e.key < widget.columns.length ? widget.columns[e.key] : '';
            return '$col: ${e.value.isEmpty ? '未知' : e.value}';
          }).join('\n');
    title = title;
    return ListTile(
      dense: true,
      contentPadding:
          const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      leading: _numBadge(globalIndex),
      title: Text(title),
      onLongPress: (widget.kind == 'user')
          ? null
          : () => _openOpSheet(cells),
    );
  }

  // Z远程协助: 长按操作面板。软件 -> 卸载；服务按 status 给出停/启用。
  void _openOpSheet(List<String> cells) {
    final name = cells.isEmpty ? '' : cells[0];
    final status = cells.length > 1 ? cells[1].toLowerCase() : '';
    final isRunning = status.contains('running');
    showModalBottomSheet(
      context: context,
      builder: (context) {
        return Container(
          padding: const EdgeInsets.all(16),
          child: ChangeNotifierProvider.value(
            value: ConfigInfoController.instance,
            child: Consumer<ConfigInfoController>(
              builder: (context, c, _) {
                return Column(
                  mainAxisSize: MainAxisSize.min,
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    Text(
                      name.isEmpty ? '操作' : name,
                      style: const TextStyle(
                          fontSize: 16, fontWeight: FontWeight.bold),
                    ),
                    const SizedBox(height: 12),
                    if (c.opPending)
                      const Padding(
                        padding: EdgeInsets.symmetric(vertical: 8),
                        child: Text('操作中...',
                            style: TextStyle(color: Colors.blue)),
                      ),
                    if (c.opResultMessage != null && !c.opPending)
                      Padding(
                        padding: const EdgeInsets.symmetric(vertical: 8),
                        child: Text(
                          c.opResultMessage!,
                          style: TextStyle(
                              color: c.opResultMessage!.contains('权限')
                                  ? Colors.red
                                  : Colors.green),
                        ),
                      ),
                    if (c.opResultMessage != null &&
                        c.opResultMessage!.contains('权限'))
                      const Padding(
                        padding: EdgeInsets.only(bottom: 8),
                        child: Text('需要管理员权限',
                            style: TextStyle(
                                color: Colors.red,
                                fontWeight: FontWeight.bold)),
                      ),
                    if (widget.kind == 'software')
                      ElevatedButton(
                        style: ElevatedButton.styleFrom(
                            backgroundColor: Colors.red),
                        onPressed: c.opPending
                            ? null
                            : () => ConfigInfoController.instance
                                .sendOp({'op': 'uninstall', 'name': name}),
                        child: const Text('卸载',
                            style: TextStyle(color: Colors.white)),
                      ),
                    if (widget.kind == 'service')
                      ElevatedButton(
                        style: ElevatedButton.styleFrom(
                            backgroundColor:
                                isRunning ? Colors.orange : Colors.green),
                        onPressed: c.opPending
                            ? null
                            : () => ConfigInfoController.instance.sendOp({
                                  'op': isRunning ? 'service_stop' : 'service_start',
                                  'name': name,
                                }),
                        child: Text(
                          isRunning ? '停用' : '启用',
                          style: const TextStyle(color: Colors.white),
                        ),
                      ),
                    TextButton(
                      onPressed: () => Navigator.of(context).pop(),
                      child: const Text('关闭'),
                    ),
                  ],
                );
              },
            ),
          ),
        );
      },
    );
  }

  @override
  Widget build(BuildContext context) {
    final filtered = _filtered;
    // 搜索框（仅 searchable Tab）。
    final searchBox = widget.searchable
        ? Padding(
            padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
            child: TextField(
              decoration: InputDecoration(
                isDense: true,
                hintText: '搜索...',
                prefixIcon: const Icon(Icons.search, size: 20),
                suffixIcon: _keyword.isEmpty
                    ? null
                    : IconButton(
                        icon: const Icon(Icons.clear, size: 18),
                        onPressed: () => setState(() {
                          _keyword = '';
                          _page = 0;
                        }),
                      ),
                border: OutlineInputBorder(
                  borderRadius: BorderRadius.circular(8),
                ),
              ),
              onChanged: (v) => setState(() {
                _keyword = v;
                _page = 0;
              }),
            ),
          )
        : null;

    if (filtered.isEmpty) {
      return Column(
        children: [
          if (searchBox != null) searchBox,
          Expanded(
            child: Center(
              child: Text(_keyword.trim().isEmpty
                  ? widget.emptyText
                  : '无匹配结果'),
            ),
          ),
        ],
      );
    }
    // 不分页（用户列表）：一次性展示，底部"共 N 条"。
    if (!widget.paginated) {
      return Column(
        children: [
          if (searchBox != null) searchBox,
          Expanded(
            child: ListView.separated(
              padding: const EdgeInsets.symmetric(vertical: 8, horizontal: 12),
              itemCount: filtered.length,
              separatorBuilder: (_, __) => const Divider(height: 1),
              itemBuilder: (context, i) => _buildRow(filtered[i], i, i),
            ),
          ),
          Padding(
            padding: const EdgeInsets.all(12),
            child: Text('共 ${filtered.length} 条'),
          ),
        ],
      );
    }
    // 分页（已安装软件 / 服务列表）：每页 100 条，底部翻页控件。
    final total = filtered.length;
    final totalPages = (total / _kConfigPageSize).ceil();
    if (_page >= totalPages) _page = totalPages - 1;
    final start = _page * _kConfigPageSize;
    final end = (start + _kConfigPageSize) > total ? total : start + _kConfigPageSize;
    final visible = filtered.sublist(start, end);
    return Column(
      children: [
        if (searchBox != null) searchBox,
        Expanded(
          child: ListView.separated(
            padding: const EdgeInsets.symmetric(vertical: 8, horizontal: 12),
            itemCount: visible.length,
            separatorBuilder: (_, __) => const Divider(height: 1),
            itemBuilder: (context, i) => _buildRow(visible[i], start + i, i),
          ),
        ),
        Container(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
          child: Row(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              TextButton(
                onPressed: _page > 0
                    ? () => setState(() => _page--)
                    : null,
                child: const Text('上一页'),
              ),
              Text('第 ${_page + 1} / $totalPages 页（共 $total 条）'),
              TextButton(
                onPressed: _page < totalPages - 1
                    ? () => setState(() => _page++)
                    : null,
                child: const Text('下一页'),
              ),
            ],
          ),
        ),
      ],
    );
  }
}
