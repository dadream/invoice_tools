# OCR 模型文件

本目录存放 PaddleOCR 的 ONNX 导出模型，**不提交到仓库**（体积大）。

需要三个文件：

- 检测模型：`ch_PP-OCRv6_det_infer.onnx`
- 识别模型：`ch_PP-OCRv6_rec_infer.onnx`
- 字典文件：`ppocr_keys_v1.txt`

## 下载来源

PaddleOCR 官方模型库：https://github.com/PaddlePaddle/PaddleOCR/blob/main/doc/doc_ch/models_list.md

PP-OCRv6 模型需要从 PaddlePaddle 格式转换为 ONNX 格式。

或使用已转换的 ONNX 版本：
- 检测模型 (small): https://paddleocr.bj.bcebos.com/PP-OCRv6/chinese/ch_PP-OCRv6_det_small_infer.tar
- 识别模型 (small): https://paddleocr.bj.bcebos.com/PP-OCRv6/chinese/ch_PP-OCRv6_rec_small_infer.tar

## 使用说明

1. 下载模型文件并放到本目录
2. 如果是 PaddlePaddle 格式 (.pdmodel/.pdparams)，需要转换为 ONNX 格式
3. 运行测试：`cargo run -p invoice-parse -- ocr fixtures/samples/<图片>`

## 文件体积

记录实际文件体积（用于评估安装包大小）：

- 检测模型 (PP-OCRv6_det_small): 待记录
- 识别模型 (PP-OCRv6_rec_small): 待记录
- 字典文件: ~40KB
