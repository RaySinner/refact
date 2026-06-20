<<<<<<< HEAD
import { Text, TextField } from "@radix-ui/themes";
import { FC, ReactNode } from "react";
import { Markdown } from "../../../../../components/Markdown";
=======
import { type FC, type ReactNode } from "react";
import { Markdown } from "../../../../../components/Markdown";
import { FieldStack, FieldText } from "../../../../../components/ui";
>>>>>>> upstream/main

type FormFieldProps = {
  label: string;
  value?: string;
  placeholder?: string;
  description?: string;
<<<<<<< HEAD
  type?: TextField.RootProps["type"];
=======
  type?: React.HTMLInputTypeAttribute;
>>>>>>> upstream/main
  isDisabled?: boolean;
  max?: string;
  onChange?: React.ChangeEventHandler<HTMLInputElement>;
  children?: ReactNode;
};

<<<<<<< HEAD
/**
 * Reusable form field component with consistent styling
 */
=======
>>>>>>> upstream/main
export const FormField: FC<FormFieldProps> = ({
  label,
  value,
  placeholder,
  description,
  isDisabled,
  type,
  max,
  onChange,
  children,
}) => {
  return (
<<<<<<< HEAD
    <label>
      <Text as="div" size="2" mb="1" weight="bold">
        {label}
      </Text>
      {description && (
        <Text as="div" size="1" color="gray" my="1">
          <Markdown>{description}</Markdown>
        </Text>
      )}
      {children ?? (
        <TextField.Root
          value={value}
          placeholder={placeholder}
          type={type}
          max={max}
          onChange={onChange}
          disabled={isDisabled}
        />
      )}
    </label>
=======
    <FieldStack
      label={label}
      helper={description ? <Markdown>{description}</Markdown> : undefined}
      control={
        children ?? (
          <FieldText
            value={value ?? ""}
            placeholder={placeholder}
            type={type}
            max={max}
            onChange={(nextValue) =>
              onChange?.({
                target: { value: nextValue },
                currentTarget: { value: nextValue },
              } as React.ChangeEvent<HTMLInputElement>)
            }
            disabled={isDisabled}
          />
        )
      }
    />
>>>>>>> upstream/main
  );
};
