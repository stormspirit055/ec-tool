"""质检程序入口：PyInstaller 打包 model-qc-worker 的 nohand 命令行。

用法与 `python -m nohand` 相同，模型路径由环境变量 NOHAND_MODEL 指定。
"""
from nohand.cli import main

main()
