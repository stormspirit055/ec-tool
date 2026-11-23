"""handpose-engine 入口：PyInstaller 打包 model-qc-worker 的两个命令行，共用同一份 MediaPipe/PyAV 运行库。

    handpose-engine <视频> --json out.json ...        手部关键点（nohand.handpose）
    handpose-engine sample <任务目录> <输出目录> ...    生成完整样例（qc_sample / nohand.sample）
    handpose-engine trim <任务目录> --videos ...       同步删除视频及传感器片段（qc_trim）

模型路径由环境变量 HANDPOSE_MODEL 指定。
"""
import sys

if len(sys.argv) > 1 and sys.argv[1] == "trim":
    from qc_trim import main

    main(sys.argv[2:])
elif len(sys.argv) > 1 and sys.argv[1] == "sample":
    from qc_sample import main

    main(sys.argv[2:])
else:
    from nohand.handpose import main

    main()
