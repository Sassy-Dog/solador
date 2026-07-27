// Serves app/ui plus a dumped view-model. No build step: the frontend is static.
export default {
  testDir: ".",
  use: { baseURL: "http://127.0.0.1:4173" },
  webServer: {
    command: "python3 -m http.server 4173 --directory ../../app/ui",
    url: "http://127.0.0.1:4173/index.html",
    reuseExistingServer: true,
  },
};
