#!/usr/bin/env python

import os

# message.proto was moved to libs/base/protos; rendezvous.proto stays in
# libs/hbb_common/protos (submodule). protoc accepts multiple -I dirs.
path_base = os.path.abspath(os.path.join(os.getcwd(), '..', '..', '..', 'libs', 'base', 'protos'))
path_hbb = os.path.abspath(os.path.join(os.getcwd(), '..', '..', '..', 'libs', 'hbb_common', 'protos'))

if os.name == 'nt':
    plugin = r'.\node_modules\.bin\protoc-gen-ts_proto.cmd'
    cmd = r'protoc --ts_proto_opt=esModuleInterop=true --ts_proto_opt=snakeToCamel=false --plugin=protoc-gen-ts_proto=%s  -I "%s" -I "%s" --ts_proto_out=./src/ rendezvous.proto'%(plugin, path_hbb, path_base)
    print(cmd)
    os.system(cmd)
    cmd = r'protoc --ts_proto_opt=esModuleInterop=true --ts_proto_opt=snakeToCamel=false --plugin=protoc-gen-ts_proto=%s  -I "%s" -I "%s" --ts_proto_out=./src/ message.proto'%(plugin, path_hbb, path_base)
    print(cmd)
    os.system(cmd)
else:
    plugin = './node_modules/.bin/protoc-gen-ts_proto'
    cmd = r'protoc --ts_proto_opt=esModuleInterop=true --ts_proto_opt=snakeToCamel=false --plugin=%s -I "%s" -I "%s" --ts_proto_out=./src/ rendezvous.proto'%(plugin, path_hbb, path_base)
    print(cmd)
    os.system(cmd)
    cmd = r'protoc --ts_proto_opt=esModuleInterop=true --ts_proto_opt=snakeToCamel=false --plugin=%s -I "%s" -I "%s" --ts_proto_out=./src/ message.proto'%(plugin, path_hbb, path_base)
    print(cmd)
    os.system(cmd)
