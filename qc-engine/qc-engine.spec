# PyInstaller 配置：把 model-qc-worker 的 nohand 命令行打包成独立程序。
# 由 scripts/build-qc-engine.mjs 调用，源码目录通过环境变量 MODEL_QC_DIR 传入。
#
# 用 onedir（不用 onefile）：onefile 每次启动都要把约 250MB 解压到临时目录。
import os

from PyInstaller.utils.hooks import collect_submodules

import sys

src = os.environ["MODEL_QC_DIR"]
# collect_submodules 在打包进程里导入源码，需要先把源码目录加入 sys.path，否则什么也找不到
sys.path.insert(0, src)

# 场景模块由注册表按名称动态导入（importlib），PyInstaller 静态分析找不到，需要显式列出。
# Web Demo、内部 HTTP 服务和手部关键点（另一个引擎）用不到，不打包。
hidden = [
    m for m in collect_submodules("nohand")
    if not m.startswith(("nohand.web", "nohand.service", "nohand.handpose"))
]
assert "nohand.scenes.no_hand" in hidden, "没有找到 nohand 场景模块，检查 MODEL_QC_DIR"

a = Analysis(
    [os.path.join(SPECPATH, "entry.py")],
    pathex=[src],
    hiddenimports=hidden,
    excludes=["tkinter", "nohand.web", "nohand.service", "matplotlib", "IPython", "pytest"],
    noarchive=False,
)
pyz = PYZ(a.pure)
exe = EXE(
    pyz,
    a.scripts,
    [],
    exclude_binaries=True,
    name="qc-engine",
    console=True,
    strip=False,
    upx=False,
)
coll = COLLECT(exe, a.binaries, a.datas, strip=False, upx=False, name="qc-engine")
