# 固定 BERT4NER 模型导出

[构建主线](../../docs/build-and-deploy.md#4-准备检测资源) · [脚本速查](../../docs/scripts.md#3-模型安装与打包) · [检测器资源说明](../../docs/rust-detector.md)

这里是生产 ONNX 资源的准备工具，和 Rust 服务分开运行。只导出 shibing624/bert4ner-base-chinese。数值检查读取 tests/fixtures/native-detector-reference.jsonl 中的 59 条 request；该文件同时用于 Rust 回归，避免维护两份样本。导出仅需本目录的工具、固定样本和原始模型权重。

所有命令从 CensorPrivate/ 目录执行。Rust 构建和已有 ONNX 包的服务运行无需本节 Python 环境；首次导出需要 Python ≥3.11 和事先取得的完整源权重。

## 准备与导出

```bash
python3 -m venv .venv-model
# CPU 环境先安装 CPU wheel，再安装已验证的直接依赖版本。
.venv-model/bin/python -m pip install 'torch==2.14.1' --index-url https://download.pytorch.org/whl/cpu
.venv-model/bin/python -m pip install -r scripts/model/requirements.txt
```

requirements.txt 固定本机实测的直接依赖，不是所有平台通用的 wheel 或完整传递依赖锁。其他版本/平台需重新通过下述导出和网关测试。不要为运行 Rust 服务而安装这些依赖。

源模型来自 https://huggingface.co/shibing624/bert4ner-base-chinese 。在联网的准备机使用 hf download 或已有镜像取得完整目录，至少包含 config.json、tokenizer_config.json、special_tokens_map.json、vocab.txt、model.safetensors。示例（hf 由上一步依赖提供）：

```bash
.venv-model/bin/hf download shibing624/bert4ner-base-chinese --revision 5d660ed2aa9da482bf2d99c6bc8cf2ce66758f6a --local-dir .artifacts/model-source/bert4ner-base-chinese
export CP_SOURCE_MODEL_DIR=$PWD/.artifacts/model-source/bert4ner-base-chinese
# 如已有权重，将该变量设为实际本地目录即可。
python3 -B scripts/model/export.py --source "$CP_SOURCE_MODEL_DIR" --check-source
export HF_HUB_OFFLINE=1 TRANSFORMERS_OFFLINE=1
.venv-model/bin/python -B scripts/model/export.py --source "$CP_SOURCE_MODEL_DIR" --output models/bert4ner-base-chinese
```

hf download 是准备步骤，Rust 服务和导出器本身不访问模型仓库。上例已固定模型 revision；--check-source 比对 validated-model.json 的源文件 SHA-256。源文件哈希是本轮验收快照，不能仅凭仓库名称认定任意后续权重一致。

导出采用 eval/eager attention、FP32、opset 17、动态 batch/sequence；所有窗口的 token argmax 必须一致，max logits error ≤1e-4，max probability error ≤2e-5。输出包括 model.onnx、tokenizer.json、config.json、manifest.json 和 export-validation.json。validated-model.json 保留已验收源文件/产物哈希及数值报告；导出结果应以本次实际生成的 manifest 和验收为准，跨工具版本不承诺字节完全相同。

## 提供原生运行库与验证

Rust 进程动态加载 libonnxruntime.so，不启动 Python。可使用 ONNX Runtime 官方 CPU 发行包，也可在准备环境中取得同版本 wheel 所带原生库：

```bash
export CP_DETECTOR_MODEL_DIR=$PWD/models/bert4ner-base-chinese
export CP_DETECTOR_ORT_LIBRARY=$(find "$PWD/.venv-model/lib" -path '*/onnxruntime/capi/libonnxruntime.so*' -type f -print -quit)
./cpctl doctor
cargo build --locked --release --bins
bash scripts/test/detector.sh --integration
CP_TEST_REAL_DETECTOR=1 bash scripts/test/native.sh
```

上例 library 路径仅用于开发机。正式部署应将模型包和平台匹配的原生库放到服务账户可读的稳定目录，分别配置 CP_DETECTOR_MODEL_DIR 和 CP_DETECTOR_ORT_LIBRARY，不依赖准备环境的 venv 路径。运行时参数和安装布局见 [检测器部署说明](../../docs/rust-detector.md)。

模型/运行库、源权重、虚拟环境和日志不进 Git。Git 保留导出工具、依赖版本、固定样本、产物结构/哈希和许可来源。完整 Python 迁移资料仅在本地保留；源码构建、导出和回归测试使用仓库内的文件即可完成。
