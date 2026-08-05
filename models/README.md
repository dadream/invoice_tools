# OCR 模型文件

本目录存放 PaddleOCR 的 ONNX 导出模型，**不提交到仓库**（体积大）。

需要三个文件（具体名称见所选 crate 文档）：

- 检测模型：`ch_PP-OCRv4_det_infer.onnx`
- 识别模型：`ch_PP-OCRv4_rec_infer.onnx`
- 字典文件：`ppocr_keys_v1.txt`

下载来源：PaddleOCR 官方模型库，或所选 crate README 提供的转换好的 ONNX 版本。

下载后记录实际文件体积到验证报告——它直接决定安装包能否控制在 30MB 内。
