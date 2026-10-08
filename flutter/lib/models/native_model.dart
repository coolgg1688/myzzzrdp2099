import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:device_info_plus/device_info_plus.dart';
import 'package:ffi/ffi.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_hbb/consts.dart';
import 'package:flutter_hbb/main.dart';
import 'package:package_info_plus/package_info_plus.dart';
import 'package:path_provider/path_provider.dart';

import '../common.dart';
import '../generated_bridge.dart';

final class RgbaFrame extends Struct {
  @Uint32()
  external int len;
  external Pointer<Uint8> data;
}

typedef F3 = Pointer<Uint8> Function(Pointer<Utf8>, int);
typedef F3Dart = Pointer<Uint8> Function(Pointer<Utf8>, Int32);
// Z远程协助: 本机「查看本机配置」——rust 侧 get_local_config_info 返回 CString 指针。
typedef F4 = Pointer<Utf8> Function();
typedef F4Dart = Pointer<Utf8> Function();
// Z远程协助: 释放 get_local_config_info 的返回值(必须回 Rust 分配器，不能 malloc.free)。
// 注意: native 签名用 dart:ffi 的 Void(大写，见 F5Native)；Dart 侧回调/绑定类型必须用小写 void(见 F5Dart)。
typedef F5Dart = void Function(Pointer<Utf8>);
typedef F5 = void Function(Pointer<Utf8>);
// Z远程协助: free_local_config_info 的 native 签名必须用 Void(大写)作为返回类型，
// 否则 NativeFunction<void Function(...)> 不是合法 NativeType，无法用于 Pointer 泛型。
typedef F5Native = Void Function(Pointer<Utf8>);
// Z远程协助: 输入授权码——rdc_auth_code(密文 CString)→结果 CString；rdc_auth_code_free 释放返回值。
typedef F6 = Pointer<Utf8> Function(Pointer<Utf8>);
typedef F6Dart = Pointer<Utf8> Function(Pointer<Utf8>);
typedef F7Native = Void Function(Pointer<Utf8>);
typedef F7Dart = void Function(Pointer<Utf8>);
typedef HandleEvent = Future<void> Function(Map<String, dynamic> evt);

// Z远程协助: 在后台 isolate 内经地址重建 get_local_config_info / free_local_config_info 指针并调用。
// 参数为 List<int> [getAddr, freeAddr] 以便经 compute() 跨 isolate 传递。
// 返回 CString 文本；任何异常返回空串(前端 toast「暂不支持/失败」)，不拖垮进程。
String _callConfigInfoByAddress(List<int> args) {
  try {
    final getAddr = args[0];
    final freeAddr = args.length > 1 ? args[1] : 0;
    final get =
        Pointer<NativeFunction<F4>>.fromAddress(getAddr).asFunction<F4Dart>();
    final p = get();
    if (p == nullptr) return '';
    try {
      return p.toDartString();
    } finally {
      if (freeAddr != 0) {
        Pointer<NativeFunction<F5Native>>.fromAddress(freeAddr)
            .asFunction<F5Dart>()(p);
      }
    }
  } catch (_) {
    return '';
  }
}

/// The Linux bundle keeps the core library at lib/librustdesk.so next to the
/// executable. Prefer that copy, mirroring flutter/linux/main.cc: the plain
/// name relies on the loader search path, which repackaged installs may not
/// cover. https://github.com/rustdesk/rustdesk/discussions/14407
DynamicLibrary _openLinuxCoreLib() {
  final bundled =
      '${File(Platform.resolvedExecutable).parent.path}/lib/librustdesk.so';
  try {
    if (File(bundled).existsSync()) {
      return DynamicLibrary.open(bundled);
    }
  } catch (e) {
    debugPrint("Failed to load '$bundled': $e");
  }
  return DynamicLibrary.open('librustdesk.so');
}

/// FFI wrapper around the native Rust core.
/// Hides the platform differences.
class PlatformFFI {
  String _dir = '';
  // _homeDir is only needed for Android and IOS.
  String _homeDir = '';
  final _eventHandlers = <String, Map<String, HandleEvent>>{};
  late RustdeskImpl _ffiBind;
  late String _appType;
  StreamEventHandler? _eventCallback;

  PlatformFFI._();

  static final PlatformFFI instance = PlatformFFI._();
  final _toAndroidChannel = const MethodChannel('mChannel');

  RustdeskImpl get ffiBind => _ffiBind;
  F3? _session_get_rgba;
  F4? _get_local_config_info;
  // Z远程协助: 底层函数指针(跨 isolate 需用地址重建)，用于后台 isolate 采集本机配置，
  // 避免同步 FFI 在 UI 线程阻塞。
  Pointer<NativeFunction<F4>>? _get_local_config_info_ptr;
  Pointer<NativeFunction<F5Native>>? _free_local_config_info_ptr;
  F5? _free_local_config_info;
  // Z远程协助: 输入授权码 FFI(rdc_auth_code / rdc_auth_code_free)。web 缺失置 null。
  F6Dart? _rdc_auth_code;
  F7Dart? _rdc_auth_code_free;

  static get localeName => Platform.localeName;

  static get isMain => instance._appType == kAppTypeMain;

  static String getByName(String name, [String arg = '']) {
    return '';
  }

  static void setByName(String name, [String value = '']) {}

  static Future<String> getVersion() async {
    PackageInfo packageInfo = await PackageInfo.fromPlatform();
    return packageInfo.version;
  }

  bool registerEventHandler(
      String eventName, String handlerName, HandleEvent handler, {bool replace = false}) {
    debugPrint('registerEventHandler $eventName $handlerName');
    var handlers = _eventHandlers[eventName];
    if (handlers == null) {
      _eventHandlers[eventName] = {handlerName: handler};
      return true;
    } else {
      if (!replace && handlers.containsKey(handlerName)) {
        return false;
      } else {
        handlers[handlerName] = handler;
        return true;
      }
    }
  }

  void unregisterEventHandler(String eventName, String handlerName) {
    debugPrint('unregisterEventHandler $eventName $handlerName');
    var handlers = _eventHandlers[eventName];
    if (handlers != null) {
      handlers.remove(handlerName);
    }
  }

  String translate(String name, String locale) =>
      _ffiBind.translate(name: name, locale: locale);

  /// Z远程协助: 本机「查看本机配置」——调用 rust 侧 get_local_config_info 获取本机
  /// 软硬件配置 JSON。符号缺失(安卓/web)时返回空串。采集已全链路 catch_unwind。
  String getLocalConfigInfo() {
    if (_get_local_config_info == null) return '';
    final p = _get_local_config_info!();
    if (p == nullptr) return '';
    try {
      return p.toDartString();
    } finally {
      // Z远程协助: 必须由 Rust 分配器释放(配套 free_local_config_info)，不可用 malloc.free。
      _free_local_config_info?.call(p);
    }
  }

  /// Z远程协助: 后台采集本机配置——把底层函数指针地址传入 compute()。
  /// native 平台 compute() 走后台 isolate，不阻塞 UI(注册表/服务/磁盘/网速/公网IP 采集
  /// 原本同步 FFI 会卡死 UI 导致「正在获取…无结果」)；web 平台 compute() 同步执行，
  /// 但 web 上符号缺失时 ptr 为 null 会直接返回空串，不会走到采集。
  Future<String> getLocalConfigInfoAsync() async {
    // Z远程协助: 主 isolate 同步调用(与远程「查看配置信息」同一已验证路径)。
    // 不能用 compute 真 isolate 跑 collect_config_info——其内部含公网IP请求等耗时操作，
    // 在 isolate 中易卡死导致前端 await 永不返回(表现为「查看本机配置」无结果)。
    if (_get_local_config_info == null) return '';
    try {
      final p = _get_local_config_info!();
      if (p == nullptr) return '';
      try {
        return p.toDartString();
      } finally {
        _free_local_config_info?.call(p);
      }
    } catch (_) {
      return '';
    }
  }

  /// Z远程协助: 输入授权码(加密后的注册码)→rust 解密+UPSERT cfg0+刷新授权选项，
  /// 返回结果字符串(成功为「授权：…；服务期至：…」，失败以「授权失败：」开头)。
  String authCode(String code) {
    if (_rdc_auth_code == null || _rdc_auth_code_free == null) {
      return '授权失败：当前平台不支持';
    }
    final c = code.toNativeUtf8();
    try {
      final p = _rdc_auth_code!(c);
      if (p == nullptr) return '授权失败：未知错误';
      try {
        return p.toDartString();
      } finally {
        _rdc_auth_code_free!(p);
      }
    } finally {
      malloc.free(c);
    }
  }

  Uint8List? getRgba(SessionID sessionId, int display, int bufSize) {
    if (_session_get_rgba == null) return null;
    final sessionIdStr = sessionId.toString();
    var a = sessionIdStr.toNativeUtf8();
    try {
      final buffer = _session_get_rgba!(a, display);
      if (buffer == nullptr) {
        return null;
      }
      final data = buffer.asTypedList(bufSize);
      return data;
    } finally {
      malloc.free(a);
    }
  }

  int getRgbaSize(SessionID sessionId, int display) =>
      _ffiBind.sessionGetRgbaSize(sessionId: sessionId, display: display);
  void nextRgba(SessionID sessionId, int display) =>
      _ffiBind.sessionNextRgba(sessionId: sessionId, display: display);
  void registerPixelbufferTexture(SessionID sessionId, int display, int ptr) =>
      _ffiBind.sessionRegisterPixelbufferTexture(
          sessionId: sessionId, display: display, ptr: ptr);
  void registerGpuTexture(SessionID sessionId, int display, int ptr) =>
      _ffiBind.sessionRegisterGpuTexture(
          sessionId: sessionId, display: display, ptr: ptr);

  /// Init the FFI class, loads the native Rust core library.
  Future<void> init(String appType) async {
    _appType = appType;
    final dylib = isAndroid
        ? DynamicLibrary.open('librustdesk.so')
        : isLinux
            ? _openLinuxCoreLib()
            : isWindows
                ? DynamicLibrary.open('librustdesk.dll')
                :
                // Use executable itself as the dynamic library for MacOS.
                // Multiple dylib instances will cause some global instances to be invalid.
                // eg. `lazy_static` objects in rust side, will be created more than once, which is not expected.
                //
                // isMacOS? DynamicLibrary.open("liblibrustdesk.dylib") :
                DynamicLibrary.process();
    debugPrint('initializing FFI $_appType');
    try {
      _session_get_rgba = dylib.lookupFunction<F3Dart, F3>("session_get_rgba");
      try {
        _get_local_config_info =
            dylib.lookupFunction<F4Dart, F4>("get_local_config_info");
        // Z远程协助: 与官方 F4 模式一致——lookup 用 NativeType(F5Native, 大写Void)，asFunction 用 Dart 类型(F5Dart, 小写void)。
        _free_local_config_info = dylib
            .lookup<NativeFunction<F5Native>>("free_local_config_info")
            .asFunction<F5Dart>();
        // Z远程协助: 同时保存底层指针，供后台 isolate 经地址重建调用(见 getLocalConfigInfoAsync)。
        _get_local_config_info_ptr =
            dylib.lookup<NativeFunction<F4>>("get_local_config_info");
        _free_local_config_info_ptr =
            dylib.lookup<NativeFunction<F5Native>>("free_local_config_info");
      } catch (_) {
        // 安卓/Web 未导出该符号，禁用「查看本机配置」。
        _get_local_config_info = null;
        _free_local_config_info = null;
        _get_local_config_info_ptr = null;
        _free_local_config_info_ptr = null;
      }
      try {
        // Z远程协助: 输入授权码 FFI。
        _rdc_auth_code = dylib.lookupFunction<F6Dart, F6>("rdc_auth_code");
        _rdc_auth_code_free = dylib
            .lookup<NativeFunction<F7Native>>("rdc_auth_code_free")
            .asFunction<F7Dart>();
      } catch (_) {
        // web 等未导出该符号，禁用「输入授权」。
        _rdc_auth_code = null;
        _rdc_auth_code_free = null;
      }
      try {
        // SYSTEM user failed
        _dir = (await getApplicationDocumentsDirectory()).path;
      } catch (e) {
        debugPrint('Failed to get documents directory: $e');
      }
      _ffiBind = RustdeskImpl(dylib);

      if (isLinux) {
        if (isMain) {
          // Start a dbus service for uri links, no need to await
          _ffiBind.mainStartDbusServer();
        }
      } else if (isMacOS && isMain) {
        // Start ipc service for uri links.
        _ffiBind.mainStartIpcUrlServer();
      }
      _startListenEvent(_ffiBind); // global event
      try {
        if (isAndroid) {
          // Android file transfer uses app-specific storage. User-selected
          // files enter and leave this workspace through the system picker.
          _homeDir = (await getExternalStorageDirectory())?.path ??
              (await getApplicationSupportDirectory()).path;
        } else if (isIOS) {
          // The previous code was `_homeDir = (await getDownloadsDirectory())?.path ?? '';`,
          // which provided the `downloads` path in the sandbox.
          // It is unclear why we now use the `data` directory in the sandbox instead.
          _homeDir = _ffiBind.mainGetDataDirIos(appDir: _dir);
        } else {
          // no need to set home dir
        }
      } catch (e) {
        debugPrintStack(label: 'initialize failed: $e');
      }
      String id = 'NA';
      String name = 'Flutter';
      DeviceInfoPlugin deviceInfo = DeviceInfoPlugin();
      if (isAndroid) {
        AndroidDeviceInfo androidInfo = await deviceInfo.androidInfo;
        name = '${androidInfo.brand}-${androidInfo.model}';
        id = androidInfo.id.hashCode.toString();
        androidVersion = androidInfo.version.sdkInt;
      } else if (isIOS) {
        IosDeviceInfo iosInfo = await deviceInfo.iosInfo;
        name = iosInfo.utsname.machine;
        id = iosInfo.identifierForVendor.hashCode.toString();
      } else if (isLinux) {
        LinuxDeviceInfo linuxInfo = await deviceInfo.linuxInfo;
        name = linuxInfo.name;
        id = linuxInfo.machineId ?? linuxInfo.id;
      } else if (isWindows) {
        try {
          // request windows build number to fix overflow on win7
          windowsBuildNumber = getWindowsTargetBuildNumber();
          WindowsDeviceInfo winInfo = await deviceInfo.windowsInfo;
          name = winInfo.computerName;
          id = winInfo.computerName;
        } catch (e) {
          debugPrintStack(label: "get windows device info failed: $e");
          name = "unknown";
          id = "unknown";
        }
      } else if (isMacOS) {
        MacOsDeviceInfo macOsInfo = await deviceInfo.macOsInfo;
        name = macOsInfo.computerName;
        id = macOsInfo.systemGUID ?? '';
      }
      if (isAndroid || isIOS) {
        debugPrint(
            '_appType:$_appType,info1-id:$id,info2-name:$name,dir:$_dir,homeDir:$_homeDir');
      } else {
        debugPrint(
            '_appType:$_appType,info1-id:$id,info2-name:$name,dir:$_dir');
      }
      if (desktopType == DesktopType.cm) {
        await _ffiBind.cmInit();
      }
      await _ffiBind.mainDeviceId(id: id);
      await _ffiBind.mainDeviceName(name: name);
      await _ffiBind.mainSetHomeDir(home: _homeDir);
      await _ffiBind.mainInit(
        appDir: _dir,
        customClientConfig: '',
      );
    } catch (e) {
      debugPrintStack(label: 'initialize failed: $e');
    }
    version = await getVersion();
  }

  Future<bool> tryHandle(Map<String, dynamic> evt) async {
    final name = evt['name'];
    if (name != null) {
      final handlers = _eventHandlers[name];
      if (handlers != null) {
        if (handlers.isNotEmpty) {
          for (var handler in handlers.values) {
            await handler(evt);
          }
          return true;
        }
      }
    }
    return false;
  }

  /// Start listening to the Rust core's events and frames.
  void _startListenEvent(RustdeskImpl rustdeskImpl) {
    final appType =
        _appType == kAppTypeDesktopRemote ? '$_appType,$kWindowId' : _appType;
    var sink = rustdeskImpl.startGlobalEventStream(appType: appType);
    sink.listen((message) {
      () async {
        try {
          Map<String, dynamic> event = json.decode(message);
          // _tryHandle here may be more flexible than _eventCallback
          if (!await tryHandle(event)) {
            if (_eventCallback != null) {
              await _eventCallback!(event);
            }
          }
        } catch (e) {
          debugPrint('json.decode fail(): $e');
        }
      }();
    });
  }

  void setEventCallback(StreamEventHandler fun) async {
    _eventCallback = fun;
  }

  void setRgbaCallback(void Function(int, Uint8List) fun) async {}

  // web only
  void setCursorDataCallback(
      void Function(String, int, int, int, int, Uint8List) fun) async {}

  // web only, decoded WebCodecs frames arriving as ready-made images
  void setVideoFrameCallback(
      Future<void> Function(int, ui.Image, bool Function()) fun) {}

  void clearVideoFrameCallback() {}

  void startDesktopWebListener() {}

  void stopDesktopWebListener() {}

  void setMethodCallHandler(FMethod callback) {
    _toAndroidChannel.setMethodCallHandler((call) async {
      callback(call.method, call.arguments);
      return null;
    });
  }

  invokeMethod(String method, [dynamic arguments]) async {
    if (!isAndroid) return Future<bool>(() => false);
    return await _toAndroidChannel.invokeMethod(method, arguments);
  }

  Future<T?> invokeMethodWithResult<T>(String method,
      [dynamic arguments]) async {
    if (!isAndroid) return null;
    return await _toAndroidChannel.invokeMethod<T>(method, arguments);
  }

  void syncAndroidServiceAppDirConfigPath() {
    invokeMethod(AndroidChannel.kSyncAppDirConfigPath, _dir);
  }

  void setFullscreenCallback(void Function(bool) fun) {}
}
