use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("EZ Vireo", 800, 600), None::<fn()>);

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        let mut batch = DrawBatch::new();

        draw_rectangle(&mut batch, Pos::new(100.0, 100.0), 200.0, 150.0, Some(RED));
        draw_circle(&mut batch, Pos::new(500.0, 300.0), 80.0, Some(BLUE));
        draw_line(&mut batch, 50.0, 50.0, 750.0, 550.0, 2.0, Some(GREEN));

        win.draw(Some(BLACK), &[&batch]);

        true
    });
}
