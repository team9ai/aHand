import { render, screen } from "@testing-library/react";
import LoginPage from "@/app/login/page";

describe("login page", () => {
  it("renders the dashboard sign-in form", () => {
    render(<LoginPage />);

    expect(screen.getByRole("heading", { name: /ahand hub/i })).toBeInTheDocument();
    expect(screen.getByLabelText(/shared password/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /sign in/i })).toBeInTheDocument();
  });
});
