import 'dart:convert';

import 'package:desktop_multi_window/desktop_multi_window.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_hbb/consts.dart';
import 'package:flutter_hbb/common.dart';
import 'package:flutter_hbb/main.dart';
import 'package:flutter_hbb/models/input_model.dart';

/// must keep the order
// ignore: constant_identifier_names
enum WindowType {
  Main,
  RemoteDesktop,
  FileTransfer,
  ViewCamera,
  PortForward,
  Terminal,
  // Z远程协助: peer config info window.
  ConfigInfo,
  Unknown
}

extension Index on int {
  WindowType get windowType {
    switch (this) {
      case 0:
        return WindowType.Main;
      case 1:
        return WindowType.RemoteDesktop;
      case 2:
        return WindowType.FileTransfer;
      case 3:
        return WindowType.ViewCamera;
      case 4:
        return WindowType.PortForward;
      case 5:
        return WindowType.Terminal;
      case 6:
        return WindowType.ConfigInfo;
      default:
        return WindowType.Unknown;
    }
  }
}

class MultiWindowCallResult {
  int windowId;
  dynamic result;

  MultiWindowCallResult(this.windowId, this.result);
}

/// Window Manager
/// mainly use it in `Main Window`
/// use it in sub window is not recommended
class RustDeskMultiWindowManager {
  RustDeskMultiWindowManager._();

  static final instance = RustDeskMultiWindowManager._();

  final Set<int> _inactiveWindows = {};
  final Set<int> _activeWindows = {};
  final List<AsyncCallback> _windowActiveCallbacks = List.empty(growable: true);
  final List<int> _remoteDesktopWindows = List.empty(growable: true);
  final List<int> _fileTransferWindows = List.empty(growable: true);
  final List<int> _viewCameraWindows = List.empty(growable: true);
  final List<int> _portForwardWindows = List.empty(growable: true);
  final List<int> _terminalWindows = List.empty(growable: true);
  // Z远程协助
  final List<int> _configInfoWindows = List.empty(growable: true);

  moveTabToNewWindow(int windowId, String peerId, String sessionId,
      WindowType windowType) async {
    var params = {
      'type': windowType.index,
      'id': peerId,
      'tab_window_id': windowId,
      'session_id': sessionId,
    };
    if (windowType == WindowType.RemoteDesktop) {
      await _newSession(
        false,
        WindowType.RemoteDesktop,
        kWindowEventNewRemoteDesktop,
        peerId,
        _remoteDesktopWindows,
        jsonEncode(params),
      );
    } else if (windowType == WindowType.ViewCamera) {
      await _newSession(
        false,
        WindowType.ViewCamera,
        kWindowEventNewViewCamera,
        peerId,
        _viewCameraWindows,
        jsonEncode(params),
      );
    }
  }

  // This function must be called in the main window thread.
  // Because the _remoteDesktopWindows is managed in that thread.
  openMonitorSession(int windowId, String peerId, int display, int displayCount,
      Rect? screenRect, int windowType) async {
    final isCamera = windowType == WindowType.ViewCamera.index;
    final windowIDs = isCamera ? _viewCameraWindows : _remoteDesktopWindows;
    if (windowIDs.length > 1) {
      for (final windowId in windowIDs) {
        if (await DesktopMultiWindow.invokeMethod(
            windowId,
            kWindowEventActiveDisplaySession,
            jsonEncode({
              'id': peerId,
              'display': display,
            }))) {
          return;
        }
      }
    }

    final displays = display == kAllDisplayValue
        ? List.generate(displayCount, (index) => index)
        : [display];
    var params = {
      'type': windowType,
      'id': peerId,
      'tab_window_id': windowId,
      'display': display,
      'displays': displays,
    };
    if (screenRect != null) {
      params['screen_rect'] = {
        'l': screenRect.left,
        't': screenRect.top,
        'r': screenRect.right,
        'b': screenRect.bottom,
      };
    }
    await _newSession(
      false,
      windowType.windowType,
      isCamera ? kWindowEventNewViewCamera : kWindowEventNewRemoteDesktop,
      peerId,
      windowIDs,
      jsonEncode(params),
      screenRect: screenRect,
    );
  }

  Future<int> newSessionWindow(
    WindowType type,
    String remoteId,
    String msg,
    List<int> windows,
    bool withScreenRect,
  ) async {
    final windowController = await DesktopMultiWindow.createWindow(msg);
    if (isWindows) {
      windowController.setInitBackgroundColor(Colors.black);
    }
    final windowId = windowController.windowId;
    if (!withScreenRect) {
      windowController
        ..setFrame(const Offset(0, 0) &
            Size(1280 + windowId * 20, 720 + windowId * 20))
        ..center()
        ..setTitle(getWindowNameWithId(
          remoteId,
          overrideType: type,
        ));
    } else {
      windowController.setTitle(getWindowNameWithId(
        remoteId,
        overrideType: type,
      ));
    }
    if (isMacOS) {
      Future.microtask(() => windowController.show());
    }
    registerActiveWindow(windowId);
    windows.add(windowId);
    return windowId;
  }

  Future<MultiWindowCallResult> _newSession(
    bool openInTabs,
    WindowType type,
    String methodName,
    String remoteId,
    List<int> windows,
    String msg, {
    Rect? screenRect,
  }) async {
    if (openInTabs) {
      if (windows.isEmpty) {
        final windowId = await newSessionWindow(
            type, remoteId, msg, windows, screenRect != null);
        return MultiWindowCallResult(windowId, null);
      } else {
        return call(type, methodName, msg);
      }
    } else {
      if (_inactiveWindows.isNotEmpty) {
        for (final windowId in windows) {
          if (_inactiveWindows.contains(windowId)) {
            if (screenRect == null) {
              await restoreWindowPosition(type,
                  windowId: windowId, peerId: remoteId);
            }
            await DesktopMultiWindow.invokeMethod(windowId, methodName, msg);
            if (methodName != kWindowEventNewRemoteDesktop) {
              WindowController.fromWindowId(windowId).show();
            }
            registerActiveWindow(windowId);
            return MultiWindowCallResult(windowId, null);
          }
        }
      }
      final windowId = await newSessionWindow(
          type, remoteId, msg, windows, screenRect != null);
      return MultiWindowCallResult(windowId, null);
    }
  }

  Future<MultiWindowCallResult> newSession(
    WindowType type,
    String methodName,
    String remoteId,
    List<int> windows, {
    String? password,
    bool? forceRelay,
    String? switchUuid,
    bool? isRDP,
    bool? isSharedPassword,
    String? connToken,
  }) async {
    var params = {
      "type": type.index,
      "id": remoteId,
      "password": password,
      "forceRelay": forceRelay
    };
    if (switchUuid != null) {
      params['switch_uuid'] = switchUuid;
    }
    if (isRDP != null) {
      params['isRDP'] = isRDP;
    }
    if (isSharedPassword != null) {
      params['isSharedPassword'] = isSharedPassword;
    }
    if (connToken != null) {
      params['connToken'] = connToken;
    }
    final msg = jsonEncode(params);

    // separate window for file transfer is not supported
    bool openInTabs = type != WindowType.RemoteDesktop ||
        mainGetLocalBoolOptionSync(kOptionOpenNewConnInTabs);

    if (windows.length > 1 || !openInTabs) {
      for (final windowId in windows) {
        if (await DesktopMultiWindow.invokeMethod(
            windowId, kWindowEventActiveSession, remoteId)) {
          return MultiWindowCallResult(windowId, null);
        }
      }
    }

    return _newSession(openInTabs, type, methodName, remoteId, windows, msg);
  }

  Future<MultiWindowCallResult> newRemoteDesktop(
    String remoteId, {
    String? password,
    bool? isSharedPassword,
    String? switchUuid,
    bool? forceRelay,
  }) async {
    return await newSession(
      WindowType.RemoteDesktop,
      kWindowEventNewRemoteDesktop,
      remoteId,
      _remoteDesktopWindows,
      password: password,
      forceRelay: forceRelay,
      switchUuid: switchUuid,
      isSharedPassword: isSharedPassword,
    );
  }

  Future<MultiWindowCallResult> newFileTransfer(
    String remoteId, {
    String? password,
    bool? isSharedPassword,
    bool? forceRelay,
    String? connToken,
  }) async {
    return await newSession(
      WindowType.FileTransfer,
      kWindowEventNewFileTransfer,
      remoteId,
      _fileTransferWindows,
      password: password,
      forceRelay: forceRelay,
      isSharedPassword: isSharedPassword,
      connToken: connToken,
    );
  }

  Future<MultiWindowCallResult> newViewCamera(
    String remoteId, {
    String? password,
    bool? isSharedPassword,
    String? switchUuid,
    bool? forceRelay,
    String? connToken,
  }) async {
    return await newSession(
      WindowType.ViewCamera,
      kWindowEventNewViewCamera,
      remoteId,
      _viewCameraWindows,
      password: password,
      forceRelay: forceRelay,
      switchUuid: switchUuid,
      isSharedPassword: isSharedPassword,
      connToken: connToken,
    );
  }

  Future<MultiWindowCallResult> newPortForward(
    String remoteId,
    bool isRDP, {
    String? password,
    bool? isSharedPassword,
    bool? forceRelay,
    String? connToken,
  }) async {
    return await newSession(
      WindowType.PortForward,
      kWindowEventNewPortForward,
      remoteId,
      _portForwardWindows,
      password: password,
      forceRelay: forceRelay,
      isRDP: isRDP,
      isSharedPassword: isSharedPassword,
      connToken: connToken,
    );
  }

  Future<MultiWindowCallResult> newTerminal(
    String remoteId, {
    String? password,
    bool? isSharedPassword,
    bool? forceRelay,
    String? connToken,
  }) async {
    // Iterate through terminal windows in reverse order to prioritize
    // the most recently added or used windows, as they are more likely
    // to have an active session.
    for (final windowId in _terminalWindows.reversed) {
      if (await DesktopMultiWindow.invokeMethod(
          windowId, kWindowEventActiveSession, remoteId)) {
        return MultiWindowCallResult(windowId, null);
      }
    }

    // Terminal windows should always create new windows, not reuse
    // This avoids the MissingPluginException when trying to invoke
    // new_terminal on an inactive window
    var params = {
      "type": WindowType.Terminal.index,
      "id": remoteId,
      "password": password,
      "forceRelay": forceRelay,
      "isSharedPassword": isSharedPassword,
      "connToken": connToken,
    };
    final msg = jsonEncode(params);

    // Always create a new window for terminal
    final windowId = await newSessionWindow(
        WindowType.Terminal, remoteId, msg, _terminalWindows, false);
    return MultiWindowCallResult(windowId, null);
  }

  // Z远程协助: 打开/复用一个独立可调大小的配置信息子窗口。
  // waitForData=true 表示主控端已有该 peer 的认证会话（密码已通过），
  // 子窗口不再发起新的 LoginRequest，仅等待主窗口通过 kWindowEventConfigInfoData 转发数据。
  Future<MultiWindowCallResult> newConfigInfo(
    String remoteId, {
    String? password,
    bool? isSharedPassword,
    bool? forceRelay,
    String? connToken,
    bool waitForData = false,
  }) async {
    // Z远程协助: 逆序复用已存在的配置信息窗口；对已关闭但残留的 windowId 做容错清理，
    // 避免 invokeMethod 抛 MissingPluginException/PlatformException 导致菜单点击无响应。
    for (final windowId in _configInfoWindows.reversed.toList()) {
      try {
        if (await DesktopMultiWindow.invokeMethod(
            windowId, kWindowEventActiveSession, remoteId)) {
          return MultiWindowCallResult(windowId, null);
        }
      } catch (_) {
        _configInfoWindows.remove(windowId);
      }
    }
    var params = {
      "type": WindowType.ConfigInfo.index,
      "id": remoteId,
      "password": password,
      "forceRelay": forceRelay,
      "isSharedPassword": isSharedPassword,
      "connToken": connToken,
      // Z远程协助: waitForData=true 时子窗口不发起新连接，仅复用已有会话的数据通道。
      "waitForData": waitForData,
    };
    final msg = jsonEncode(params);
    final windowId = await newSessionWindow(
        WindowType.ConfigInfo, remoteId, msg, _configInfoWindows, false);
    return MultiWindowCallResult(windowId, null);
  }

  // Z远程协助: 返回当前所有配置信息子窗口 id 的只读副本，供主窗口把被控端回传的数据广播给它们。
  List<int> getConfigInfoWindows() => List.of(_configInfoWindows);

  // Z远程协助: 把被控端回传的配置信息/操作结果 JSON 广播给所有配置信息子窗口；
  // 逆序尝试，任一窗口成功送达即返回 true。供主窗口 isolate 直接调用（避免对自身
  // windowId 的 invokeMethod 自调用，desktop_multi_window 插件对 self-invoke 行为不可靠）。
  Future<bool> forwardToConfigInfoWindows(dynamic args) async {
    for (final wId in _configInfoWindows.reversed.toList()) {
      try {
        await DesktopMultiWindow.invokeMethod(wId, kWindowEventConfigInfoData, args);
        return true;
      } catch (_) {}
    }
    return false;
  }

  Future<MultiWindowCallResult> call(
      WindowType type, String methodName, dynamic args) async {
    final wnds = _findWindowsByType(type);
    if (wnds.isEmpty) {
      return MultiWindowCallResult(kInvalidWindowId, null);
    }
    for (final windowId in wnds) {
      if (_activeWindows.contains(windowId)) {
        final res =
            await DesktopMultiWindow.invokeMethod(windowId, methodName, args);
        return MultiWindowCallResult(windowId, res);
      }
    }
    final res =
        await DesktopMultiWindow.invokeMethod(wnds[0], methodName, args);
    return MultiWindowCallResult(wnds[0], res);
  }

  List<int> _findWindowsByType(WindowType type) {
    switch (type) {
      case WindowType.Main:
        return [kMainWindowId];
      case WindowType.RemoteDesktop:
        return _remoteDesktopWindows;
      case WindowType.FileTransfer:
        return _fileTransferWindows;
      case WindowType.ViewCamera:
        return _viewCameraWindows;
      case WindowType.PortForward:
        return _portForwardWindows;
      case WindowType.Terminal:
        return _terminalWindows;
      case WindowType.ConfigInfo:
        return _configInfoWindows;
      case WindowType.Unknown:
        break;
    }
    return [];
  }

  void clearWindowType(WindowType type) {
    switch (type) {
      case WindowType.Main:
        return;
      case WindowType.RemoteDesktop:
        _remoteDesktopWindows.clear();
        break;
      case WindowType.FileTransfer:
        _fileTransferWindows.clear();
        break;
      case WindowType.ViewCamera:
        _viewCameraWindows.clear();
        break;
      case WindowType.PortForward:
        _portForwardWindows.clear();
        break;
      case WindowType.Terminal:
        _terminalWindows.clear();
        break;
      case WindowType.ConfigInfo:
        _configInfoWindows.clear();
        break;
      case WindowType.Unknown:
        break;
    }
  }

  void setMethodHandler(
      Future<dynamic> Function(MethodCall call, int fromWindowId)? handler) {
    DesktopMultiWindow.setMethodHandler(handler);
  }

  Future<void> closeAllSubWindows() async {
    await Future.wait(WindowType.values.map((e) => _closeWindows(e)));
  }

  Future<void> _closeWindows(WindowType type) async {
    if (type == WindowType.Main) {
      // skip main window, use window manager instead
      return;
    }

    List<int> windows = [];
    try {
      windows = _findWindowsByType(type);
    } catch (e) {
      debugPrint('Failed to getAllSubWindowIds of $type, $e');
      return;
    }

    if (windows.isEmpty) {
      return;
    }
    for (int i = 0; i < windows.length; i++) {
      final wId = windows[i];
      final shouldSavePos = type != WindowType.Terminal || i == windows.length - 1;
      if (shouldSavePos) {
        debugPrint("closing multi window, type: ${type.toString()} id: $wId");
        try {
          await saveWindowPosition(type, windowId: wId);
        } catch (e) {
          debugPrint('Failed to save window position of $wId, $e');
        }
      }
      try {
        await WindowController.fromWindowId(wId).setPreventClose(false);
        await WindowController.fromWindowId(wId).close();
        _activeWindows.remove(wId);
      } catch (e) {
        debugPrint("$e");
        return;
      }
    }
    clearWindowType(type);
  }

  Future<List<int>> getAllSubWindowIds() async {
    try {
      final windows = await DesktopMultiWindow.getAllSubWindowIds();
      return windows;
    } catch (err) {
      if (err is AssertionError) {
        return [];
      } else {
        rethrow;
      }
    }
  }

  Set<int> getActiveWindows() {
    return _activeWindows;
  }

  Future<void> _notifyActiveWindow() async {
    for (final callback in _windowActiveCallbacks) {
      await callback.call();
    }
  }

  Future<void> registerActiveWindow(int windowId) async {
    _activeWindows.add(windowId);
    _inactiveWindows.remove(windowId);
    await _notifyActiveWindow();
  }

  /// Remove active window which has [`windowId`]
  ///
  /// [Availability]
  /// This function should only be called from main window.
  /// For other windows, please post a unregister(hide) event to main window handler:
  /// `rustDeskWinManager.call(WindowType.Main, kWindowEventHide, {"id": windowId!});`
  Future<void> unregisterActiveWindow(int windowId) async {
    _activeWindows.remove(windowId);
    if (windowId != kMainWindowId) {
      _inactiveWindows.add(windowId);
    }
    await _notifyActiveWindow();
  }

  void registerActiveWindowListener(AsyncCallback callback) {
    _windowActiveCallbacks.add(callback);
  }

  void unregisterActiveWindowListener(AsyncCallback callback) {
    _windowActiveCallbacks.remove(callback);
  }

  // This function is called from the main window.
  // It will query the active remote windows to get their coords.
  Future<List<String>> getOtherRemoteWindowCoords(int wId) async {
    List<String> coords = [];
    for (final windowId in _remoteDesktopWindows) {
      if (windowId != wId) {
        if (_activeWindows.contains(windowId)) {
          final res = await DesktopMultiWindow.invokeMethod(
              windowId, kWindowEventRemoteWindowCoords, '');
          if (res != null) {
            coords.add(res);
          }
        }
      }
    }
    return coords;
  }

  // This function is called from one remote window.
  // Only the main window knows `_remoteDesktopWindows` and `_activeWindows`.
  // So we need to call the main window to get the other remote windows' coords.
  Future<List<RemoteWindowCoords>> getOtherRemoteWindowCoordsFromMain() async {
    List<RemoteWindowCoords> coords = [];
    // Call the main window to get the coords of other remote windows.
    String res = await DesktopMultiWindow.invokeMethod(
        kMainWindowId, kWindowEventRemoteWindowCoords, kWindowId.toString());
    List<dynamic> list = jsonDecode(res);
    for (var item in list) {
      coords.add(RemoteWindowCoords.fromJson(jsonDecode(item)));
    }
    return coords;
  }
}

final rustDeskWinManager = RustDeskMultiWindowManager.instance;
