export function AssistantView() {
  return (
    <section className="assistant">
      <header className="assistant-header">
        <h1>Luna</h1>
        <span className="chip">Microphone off</span>
      </header>

      <div className="presence">
        <div className="orb" data-state="idle" aria-hidden="true" />
        <p className="status">Not listening</p>
        <p className="detail">Voice and text requests aren't available yet.</p>
      </div>

      <form className="composer" onSubmit={(event) => event.preventDefault()}>
        <input
          className="input"
          aria-label="Message"
          placeholder="Ask Luna"
          disabled
        />
      </form>
    </section>
  );
}
