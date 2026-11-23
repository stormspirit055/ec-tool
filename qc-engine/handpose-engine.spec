# PyInstaller 配置：把 model-qc-worker 的 nohand.handpose（MediaPipe 手部关键点）打包成独立程序。
# 由 scripts/build-qc-engine.mjs 调用，源码目录通过环境变量 MODEL_QC_DIR 传入。
# MediaPipe 依赖的 OpenCV 与质检固定的版本冲突，所以和 qc-engine 分开打包，各用各的虚拟环境。
import os

from PyInstaller.utils.hooks import collect_data_files, collect_dynamic_libs

src = os.environ["MODEL_QC_DIR"]

a = Analysis(
    [os.path.join(SPECPATH, "handpose-entry.py")],
    pathex=[src, SPECPATH],
    hiddenimports=["nohand.handpose", "nohand.sample", "nohand.color"],
    # MediaPipe 运行时要读包内的 .binarypb 图定义和原生库
    datas=collect_data_files("mediapipe"),
    binaries=collect_dynamic_libs("mediapipe"),
    # mediapipe 运行时会导入 matplotlib（绘图工具模块），不能排除；
    # nohand/__init__.py 会导入 nohand.scenes（只是场景目录，不加载模型），也要保留
    excludes=["tkinter", "IPython", "pytest", "nohand.web", "nohand.service",
              "nohand.rtdetr", "onnxruntime"],
    noarchive=False,
)
pyz = PYZ(a.pure)
exe = EXE(pyz, a.scripts, [], exclude_binaries=True, name="handpose-engine",
          console=True, strip=False, upx=False)
coll = COLLECT(exe, a.binaries, a.datas, strip=False, upx=False, name="handpose-engine")
