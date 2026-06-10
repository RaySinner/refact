import React, { useMemo, useState } from "react";
import { Box, IconButton, Tooltip } from "@radix-ui/themes";
import { CopyIcon, CodeIcon, EyeOpenIcon } from "@radix-ui/react-icons";
import { PreTag } from "./Pre";
import styles from "./Markdown.module.css";
import diagramStyles from "./DiagramBlock.module.css";
import classNames from "classnames";
import DOMPurify from "dompurify";

export type SvgBlockProps = {
  code: string;
  onCopyClick?: (str: string) => void;
};

const _SvgBlock: React.FC<SvgBlockProps> = ({ code, onCopyClick }) => {
  const [showSource, setShowSource] = useState(false);

  const sanitizedSvg = useMemo(() => {
    const trimmed = code.trim();
    if (!trimmed.includes("<svg")) return null;

    return DOMPurify.sanitize(trimmed, {
      USE_PROFILES: { svg: true, svgFilters: true },
      ADD_TAGS: [
        "svg",
        "path",
        "circle",
        "rect",
        "line",
        "polyline",
        "polygon",
        "ellipse",
        "g",
        "defs",
        "use",
        "text",
        "tspan",
        "marker",
        "clipPath",
        "mask",
        "pattern",
        "image",
        "linearGradient",
        "radialGradient",
        "stop",
        "filter",
        "feGaussianBlur",
        "feOffset",
        "feMerge",
        "feMergeNode",
        "feFlood",
        "feComposite",
        "feBlend",
        "animate",
        "animateTransform",
        "animateMotion",
        "foreignObject",
        "title",
        "desc",
        "symbol",
      ],
      ADD_ATTR: [
        "viewBox",
        "xmlns",
        "fill",
        "stroke",
        "stroke-width",
        "stroke-linecap",
        "stroke-linejoin",
        "stroke-dasharray",
        "stroke-dashoffset",
        "opacity",
        "transform",
        "d",
        "cx",
        "cy",
        "r",
        "rx",
        "ry",
        "x",
        "x1",
        "x2",
        "y",
        "y1",
        "y2",
        "width",
        "height",
        "points",
        "text-anchor",
        "dominant-baseline",
        "font-size",
        "font-family",
        "font-weight",
        "letter-spacing",
        "clip-path",
        "mask",
        "marker-start",
        "marker-mid",
        "marker-end",
        "gradientUnits",
        "gradientTransform",
        "offset",
        "stop-color",
        "stop-opacity",
        "patternUnits",
        "patternTransform",
        "preserveAspectRatio",
        "href",
        "xlink:href",
        "filter",
        "flood-color",
        "flood-opacity",
        "stdDeviation",
        "dx",
        "dy",
        "result",
        "in",
        "in2",
        "mode",
        "type",
        "values",
        "dur",
        "repeatCount",
        "from",
        "to",
        "begin",
        "fill-rule",
        "clip-rule",
        "color",
      ],
    });
  }, [code]);

  if (!sanitizedSvg) {
    return (
      <Box className={styles.shiki_wrapper}>
        <PreTag className={styles.shiki_pre}>
          <code className={classNames(styles.code, styles.code_block)}>
            {code}
          </code>
        </PreTag>
      </Box>
    );
  }

  return (
    <Box className={styles.shiki_wrapper}>
      <Box className={diagramStyles.diagram_container}>
        <Box className={diagramStyles.diagram_toolbar}>
          <Tooltip content={showSource ? "Show rendered" : "Show source"}>
            <IconButton
              size="1"
              variant="ghost"
              onClick={() => setShowSource((v) => !v)}
              aria-label={showSource ? "Show rendered" : "Show source"}
            >
              {showSource ? (
                <EyeOpenIcon width={12} height={12} />
              ) : (
                <CodeIcon width={12} height={12} />
              )}
            </IconButton>
          </Tooltip>
          {onCopyClick && (
            <Tooltip content="Copy SVG">
              <IconButton
                size="1"
                variant="ghost"
                onClick={() => onCopyClick(code)}
                aria-label="Copy SVG source"
              >
                <CopyIcon width={12} height={12} />
              </IconButton>
            </Tooltip>
          )}
        </Box>
        {showSource ? (
          <PreTag className={styles.shiki_pre}>
            <code className={classNames(styles.code, styles.code_block)}>
              {code}
            </code>
          </PreTag>
        ) : (
          <Box
            className={diagramStyles.diagram_render}
            dangerouslySetInnerHTML={{ __html: sanitizedSvg }}
          />
        )}
      </Box>
    </Box>
  );
};

export const SvgBlock = React.memo(_SvgBlock);
