import type { ComponentProps, FC } from "react";

export const BranchIcon: FC<ComponentProps<"svg">> = ({
  width = 14,
  height = 14,
  ...props
}) => (
  <svg
    width={width}
    height={height}
    viewBox="0 0 16 16"
    fill="none"
    aria-hidden="true"
    {...props}
  >
    <circle cx="4" cy="3.5" r="1.5" stroke="currentColor" strokeWidth="1.3" />
    <circle cx="4" cy="12.5" r="1.5" stroke="currentColor" strokeWidth="1.3" />
    <circle cx="12" cy="5.5" r="1.5" stroke="currentColor" strokeWidth="1.3" />
    <path
      d="M4 5v6M5.5 12.5h2.25A4.25 4.25 0 0 0 12 8.25V7"
      stroke="currentColor"
      strokeWidth="1.3"
      strokeLinecap="round"
      strokeLinejoin="round"
    />
  </svg>
);
