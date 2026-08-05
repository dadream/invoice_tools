#!/usr/bin/env python3
"""
OCR Sidecar for invoice-parse
Extracts text boxes from images using PaddleOCR
"""
import sys
import json

def main():
    if len(sys.argv) < 2:
        print("[]", flush=True)
        sys.exit(0)

    image_path = sys.argv[1]

    try:
        from paddleocr import PaddleOCR

        # Initialize PaddleOCR with Chinese support
        ocr = PaddleOCR(use_angle_cls=True, lang='ch', use_gpu=False)

        # Perform OCR
        result = ocr.ocr(image_path, cls=True)

        # Handle empty results
        if not result or not result[0]:
            print("[]", flush=True)
            sys.exit(0)

        # Convert to TextBox format
        boxes = []
        for line in result[0]:
            box_coords = line[0]  # [[x1,y1], [x2,y2], [x3,y3], [x4,y4]]
            text, conf = line[1]

            # Calculate bounding box from polygon
            x = min(pt[0] for pt in box_coords)
            y = min(pt[1] for pt in box_coords)
            width = max(pt[0] for pt in box_coords) - x
            height = max(pt[1] for pt in box_coords) - y

            boxes.append({
                "text": text,
                "x": float(x),
                "y": float(y),
                "width": float(width),
                "height": float(height),
                "confidence": float(conf)
            })

        # Output as JSON
        print(json.dumps(boxes, ensure_ascii=False), flush=True)

    except ImportError:
        # PaddleOCR not installed
        print("[]", flush=True)
        sys.exit(1)
    except Exception as e:
        # Any other error - output empty array
        print("[]", flush=True)
        sys.exit(1)

if __name__ == "__main__":
    main()
