import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_hbb/common.dart';
import 'package:flutter_hbb/consts.dart';
import 'package:flutter_hbb/utils/multi_window_manager.dart';
import 'package:provider/provider.dart';

import 'package:flutter_hbb/common/widgets/config_info_page.dart';

/// Z远程协助: independent resizable window hosting the peer config info.
/// 改为 StatefulWidget：必须在本 isolate 注册 method handler，否则主窗口
/// multi_window_manager.newConfigInfo 复用循环里的 kWindowEventActiveSession 调用会
/// 因 MissingPluginException/返回 null 而异常冒泡，表现为菜单「查看配置信息」点击无响应。
class DesktopConfigInfoScreen extends StatefulWidget {
  final Map<String, dynamic> params;

  const DesktopConfigInfoScreen({Key? key, required this.params})
      : super(key: key);

  @override
  State<DesktopConfigInfoScreen> createState() =>
      _DesktopConfigInfoScreenState();
}

class _DesktopConfigInfoScreenState extends State<DesktopConfigInfoScreen> {
  // Z远程协助: 本窗口 id，由 main.dart:63 注入 params['windowId']。
  int windowId() => widget.params['windowId'];

  @override
  void initState() {
    super.initState();
    rustDeskWinManager.setMethodHandler((call, fromWindowId) async {
      // Z远程协助: kWindowEventActiveSession —— 主窗口复用窗口时询问本窗口是否属于该 peer。
      if (call.method == kWindowEventActiveSession) {
        final remoteId = call.arguments?.toString() ?? '';
        if (remoteId == widget.params['id']) {
          windowOnTop(windowId());
          return true;
        }
        return false;
      }
      // Z远程协助: kWindowEventConfigInfoData —— 主窗口转发来的配置信息/操作结果 JSON。
      else if (call.method == kWindowEventConfigInfoData) {
        try {
          final decoded = jsonDecode(call.arguments.toString());
          if (decoded is Map) {
            final type = decoded['type']?.toString() ?? '';
            final text = decoded['text']?.toString() ?? '';
            if (type == 'zremote66-config-info') {
              ConfigInfoController.instance.update(text);
            } else if (type == 'zremote66-config-op-result') {
              ConfigInfoController.instance.onOpResult(text);
            }
          }
        } catch (_) {}
        return null;
      }
      // Z远程协助: 窗口销毁前关闭本 isolate 自己的 config-info 会话（幂等）。
      // waitForData 模式下本 isolate 未发起会话，gFFI.close 内部 sessionClose 对不存在的
      // session id 是空操作，无副作用。
      else if (call.method == "onDestroy") {
        unawaited(gFFI.close());
        return null;
      }
      // 其余事件忽略。
      return null;
    });
    // Z远程协助: 恢复窗口位置（main.dart runMultiWindow 也做了一次，这里保持与其它子窗口一致）。
    Future.delayed(Duration.zero, () {
      restoreWindowPosition(WindowType.ConfigInfo, windowId: windowId());
    });
  }

  @override
  Widget build(BuildContext context) {
    // Z远程协助: waitForData=true 时不发起新连接，仅等待主窗口转发数据。
    final waitForData = widget.params['waitForData'] == true;
    return MultiProvider(
      providers: [
        ChangeNotifierProvider.value(value: gFFI.ffiModel),
      ],
      child: ConfigInfoPage(
        id: widget.params['id'],
        password: widget.params['password'],
        isSharedPassword: widget.params['isSharedPassword'],
        forceRelay: widget.params['forceRelay'],
        waitForData: waitForData,
      ),
    );
  }
}
