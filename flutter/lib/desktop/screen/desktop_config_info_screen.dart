import 'package:flutter/material.dart';
import 'package:flutter_hbb/common.dart';
import 'package:provider/provider.dart';

import 'package:flutter_hbb/common/widgets/config_info_page.dart';

/// Z远程协助: independent resizable window hosting the peer config info.
class DesktopConfigInfoScreen extends StatelessWidget {
  final Map<String, dynamic> params;

  const DesktopConfigInfoScreen({Key? key, required this.params})
      : super(key: key);

  @override
  Widget build(BuildContext context) {
    return MultiProvider(
      providers: [
        ChangeNotifierProvider.value(value: gFFI.ffiModel),
      ],
      child: ConfigInfoPage(
        id: params['id'],
        password: params['password'],
        isSharedPassword: params['isSharedPassword'],
        forceRelay: params['forceRelay'],
      ),
    );
  }
}
