import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";

afterEach(() => cleanup());

// jsdom lacks these; the UI uses them for focus handling and scrolling.
if (!Element.prototype.scrollIntoView) Element.prototype.scrollIntoView = () => {};
import "@testing-library/jest-dom/vitest";
