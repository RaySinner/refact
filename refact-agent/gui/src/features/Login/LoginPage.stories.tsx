import type { Meta, StoryObj } from "@storybook/react";
import { LoginPage } from "./LoginPage";
import { Provider } from "react-redux";
import { setUpStore } from "../../app/store";
import { Theme } from "../../components/Theme";

const App = () => {
  const store = setUpStore({
    config: {
      apiKey: null,
      host: "web",
      lspPort: 8001,
      themeProps: { appearance: "dark", accentColor: "gray" },
    },
  });
  return (
    <Provider store={store}>
      <Theme>
        <LoginPage />
      </Theme>
    </Provider>
  );
};

const meta: Meta<typeof App> = {
  title: "Login",
  component: App,
} satisfies Meta<typeof LoginPage>;

export default meta;

type Story = StoryObj<typeof meta>;

export const Primary: Story = {
  args: {},
};
