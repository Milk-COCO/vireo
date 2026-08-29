use vireo::prelude::*;

#[vireo::main]
async fn main(app: App) {
    let idx = app.window(WindowDesc::new("EZ Vireo", 800, 600), None::<fn()>);

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(v) => v,
            Err(_) => return true,
        };

        let mut batch = DrawBatch::new();

        draw_text(&mut batch.texts, &format!("{:.0}", ctx.fps()), Pos::new(10.,10.), TextDef::default(), TextOverride::new());
        draw_rectangle(&mut batch, Pos::new(100.0, 100.0), 200.0, 150.0, Some(RED));
        draw_circle(&mut batch, Pos::new(500.0, 300.0), 80.0, Some(BLUE));
        draw_line(&mut batch, 50.0, 50.0, 750.0, 550.0, 2.0, Some(GREEN));

        win.draw(BLACK, &[&batch]);

        true
    }).await.unwrap();
}
