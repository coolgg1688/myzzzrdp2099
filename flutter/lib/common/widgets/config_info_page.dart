import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_hbb/common.dart';
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

  void update(String raw) {
    // Empty payload (collection failed on the peer) -> every tab falls back to
    // "暂不支持" instead of surfacing a parse error.
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
    notifyListeners();
  }
}

/// Full-screen (mobile) / embedded (desktop) tabbed view of the peer's
/// hardware/software configuration.
class ConfigInfoPage extends StatefulWidget {
  final String id;
  final String? password;
  final bool? isSharedPassword;
  final bool? forceRelay;

  const ConfigInfoPage({
    Key? key,
    required this.id,
    this.password,
    this.isSharedPassword,
    this.forceRelay,
  }) : super(key: key);

  @override
  State<ConfigInfoPage> createState() => _ConfigInfoPageState();
}

class _ConfigInfoPageState extends State<ConfigInfoPage> {
  @override
  void initState() {
    super.initState();
    ConfigInfoController.instance.reset();
    gFFI.ffiModel.updateEventListener(gFFI.sessionId, widget.id);
    gFFI.start(
      widget.id,
      isConfigInfo: true,
      password: widget.password,
      isSharedPassword: widget.isSharedPassword,
      forceRelay: widget.forceRelay,
    );
    WidgetsBinding.instance.addPostFrameCallback((_) {
      gFFI.dialogManager.showLoading('正在连接被控端...', onCancel: () {
        gFFI.close();
      });
    });
  }

  @override
  void dispose() {
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('被控端配置信息'),
      ),
      body: ChangeNotifierProvider.value(
        value: ConfigInfoController.instance,
        child: Consumer<ConfigInfoController>(
          builder: (context, c, _) {
            if (c.loading) {
              return const Center(child: CircularProgressIndicator());
            }
            if (c.error != null) {
              return Center(child: Text(c.error!));
            }
            return _buildTabs(context, c.data!);
          },
        ),
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
                _listTab(
                  rows: _userRows(data['users']),
                  columns: const ['名称', '全名', '管理员'],
                  emptyText: '暂不支持',
                ),
                _listTab(
                  rows: _rows(data['software'], ['name', 'version', 'publisher']),
                  columns: const ['名称', '版本', '发布者'],
                  emptyText: '暂不支持',
                ),
                _listTab(
                  rows: _rows(data['services'], ['name', 'status', 'start_type']),
                  columns: const ['名称', '状态', '启动类型'],
                  emptyText: '暂不支持',
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

  String _unknown(String s) => s.isEmpty ? '未知' : s;

  Widget _hardwareTab(Map<String, dynamic> data) {
    final os = (data['os'] as Map?) ?? {};
    final cpu = (data['cpu'] as Map?) ?? {};
    final mem = (data['memory'] as Map?) ?? {};
    final disk = (data['disk'] as Map?) ?? {};
    final board = (data['board'] as Map?) ?? {};

    String osLine() {
      final parts = [
        os['name']?.toString() ?? '',
        os['version']?.toString() ?? '',
        os['arch']?.toString() ?? '',
      ].where((s) => s.isNotEmpty).join(' ');
      return _unknown(parts);
    }

    String cpuLine() {
      final parts = <String>[
        cpu['model']?.toString() ?? '',
      ].where((s) => s.isNotEmpty).toList();
      final cores = (cpu['cores'] ?? 0);
      final threads = (cpu['threads'] ?? 0);
      final freq = (cpu['freq_mhz'] ?? 0);
      final detail = '核心:$cores 线程:$threads 主频:${freq}MHz';
      parts.add(detail);
      return _unknown(parts.join(' / '));
    }

    String memLine() =>
        '总:${(mem['total_gb'] ?? 0.0)}GB 可用:${(mem['available_gb'] ?? 0.0)}GB';
    String diskLine() =>
        '总:${(disk['total_gb'] ?? 0.0)}GB 剩余:${(disk['free_gb'] ?? 0.0)}GB';

    final rows = <String>[
      osLine(),
      cpuLine(),
      memLine(),
      diskLine(),
      _unknown(data['gpu']?.toString() ?? ''),
      _unknown([
        board['vendor']?.toString() ?? '',
        board['model']?.toString() ?? '',
      ].where((s) => s.isNotEmpty).join(' / ')),
      _unknown(data['screen']?.toString() ?? ''),
      _unknown(data['uptime']?.toString() ?? ''),
    ];

    return ListView.separated(
      padding: const EdgeInsets.all(12),
      itemCount: rows.length,
      separatorBuilder: (_, __) => const Divider(height: 1),
      itemBuilder: (context, i) {
        return ListTile(
          dense: true,
          leading: SizedBox(
            width: 28,
            child: Text(
              '${i + 1}',
              style: const TextStyle(fontWeight: FontWeight.bold),
            ),
          ),
          title: Text(rows[i]),
        );
      },
    );
  }

  Widget _listTab({
    required List<List<String>> rows,
    required List<String> columns,
    required String emptyText,
  }) {
    if (rows.isEmpty) {
      return Center(child: Text(emptyText));
    }
    return Column(
      children: [
        Expanded(
          child: ListView.separated(
            padding: const EdgeInsets.all(12),
            itemCount: rows.length,
            separatorBuilder: (_, __) => const Divider(height: 1),
            itemBuilder: (context, i) {
              final cells = rows[i];
              final title = cells.isEmpty
                  ? ''
                  : cells
                      .asMap()
                      .entries
                      .map((e) =>
                          '${columns[e.key]}: ${e.value.isEmpty ? '未知' : e.value}')
                      .join('\n');
              return ListTile(
                dense: true,
                leading: SizedBox(
                  width: 28,
                  child: Text(
                    '${i + 1}',
                    style: const TextStyle(fontWeight: FontWeight.bold),
                  ),
                ),
                title: Text(title),
              );
            },
          ),
        ),
        Padding(
          padding: const EdgeInsets.all(12),
          child: Text('共 ${rows.length} 条'),
        ),
      ],
    );
  }
}
