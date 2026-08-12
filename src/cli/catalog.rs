use crate::body::{build_body, BodyInput};
use crate::cli::kernel::{confirm_destructive, connect, emit};
use crate::cli::GlobalFlags;
use crate::error::Result;
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum CatalogSub {
    /// List service catalogs.
    List(CatalogListArgs),
    /// Get a specific catalog.
    Get(CatalogGetArgs),
    /// List categories for a catalog.
    Categories(CatalogCategoriesArgs),
    /// Get a specific category.
    Category(CatalogCategoryArgs),
    /// List catalog items.
    Items(CatalogItemsArgs),
    /// Get a specific catalog item.
    Item(CatalogItemArgs),
    /// Get variables for a catalog item.
    ItemVariables(CatalogItemArgs),
    /// Order a catalog item immediately.
    Order(CatalogOrderArgs),
    /// Add a catalog item to cart.
    AddToCart(CatalogOrderArgs),
    /// Get the current cart.
    Cart,
    /// Update a cart item.
    CartUpdate(CatalogCartUpdateArgs),
    /// Remove an item from cart.
    CartRemove(CatalogCartItemArgs),
    /// Empty the cart.
    CartEmpty(CatalogCartEmptyArgs),
    /// Check out the cart.
    Checkout,
    /// Submit the cart order.
    SubmitOrder,
    /// Get the wishlist.
    Wishlist,
}

#[derive(clap::Args, Debug)]
pub struct CatalogListArgs {
    /// Search text for catalogs.
    #[arg(long, alias = "sysparm-text")]
    pub text: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct CatalogGetArgs {
    /// sys_id of the catalog.
    pub sys_id: String,
}

#[derive(clap::Args, Debug)]
pub struct CatalogCategoriesArgs {
    /// Catalog sys_id.
    pub catalog_sys_id: String,
    /// Maximum records returned. Maps to sysparm_limit. Also spelled --limit or --setLimit.
    #[arg(
        long,
        alias = "sysparm-limit",
        alias = "limit",
        alias = "setLimit",
        default_value_t = 100
    )]
    pub setlimit: u32,
    /// Starting offset for manual pagination.
    #[arg(long, alias = "sysparm-offset")]
    pub offset: Option<u32>,
    /// Show only top-level categories.
    #[arg(long, alias = "sysparm-top-level-only")]
    pub top_level_only: bool,
}

#[derive(clap::Args, Debug)]
pub struct CatalogCategoryArgs {
    /// sys_id of the category.
    pub sys_id: String,
}

#[derive(clap::Args, Debug)]
pub struct CatalogItemsArgs {
    /// Search text for items.
    #[arg(long, alias = "sysparm-text")]
    pub text: Option<String>,
    /// Filter by category sys_id.
    #[arg(long, alias = "sysparm-category")]
    pub category: Option<String>,
    /// Filter by catalog sys_id.
    #[arg(long, alias = "sysparm-catalog")]
    pub catalog: Option<String>,
    /// Filter by type (e.g. `record_producer`).
    #[arg(long, alias = "sysparm-type")]
    pub item_type: Option<String>,
    /// Maximum records returned. Maps to sysparm_limit. Also spelled --limit or --setLimit.
    #[arg(
        long,
        alias = "sysparm-limit",
        alias = "limit",
        alias = "setLimit",
        default_value_t = 100
    )]
    pub setlimit: u32,
    /// Starting offset for manual pagination.
    #[arg(long, alias = "sysparm-offset")]
    pub offset: Option<u32>,
}

#[derive(clap::Args, Debug)]
pub struct CatalogItemArgs {
    /// sys_id of the catalog item.
    pub sys_id: String,
}

#[derive(clap::Args, Debug)]
pub struct CatalogOrderArgs {
    /// sys_id of the catalog item.
    pub sys_id: String,
    /// Body source: inline JSON, @file (path), or @- (stdin). Use a file to avoid shell quoting on multi-line values.
    #[arg(long, short = 'D', conflicts_with = "field")]
    pub data: Option<String>,
    /// Repeatable name=value. Use name=@file to read the value from a file (e.g. multi-line text). Mutually exclusive with --data.
    #[arg(long = "field", short = 'F', conflicts_with = "data")]
    pub field: Vec<String>,
}

#[derive(clap::Args, Debug)]
pub struct CatalogCartUpdateArgs {
    /// sys_id of the cart item.
    pub cart_item_id: String,
    /// Body source: inline JSON, @file (path), or @- (stdin). Use a file to avoid shell quoting on multi-line values.
    #[arg(long, short = 'D', conflicts_with = "field")]
    pub data: Option<String>,
    /// Repeatable name=value. Use name=@file to read the value from a file (e.g. multi-line text). Mutually exclusive with --data.
    #[arg(long = "field", short = 'F', conflicts_with = "data")]
    pub field: Vec<String>,
}

#[derive(clap::Args, Debug)]
pub struct CatalogCartItemArgs {
    /// sys_id of the cart item.
    pub cart_item_id: String,
    /// Skip confirmation prompt (required for non-interactive use).
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(clap::Args, Debug)]
pub struct CatalogCartEmptyArgs {
    /// Cart sys_id.
    pub sys_id: String,
    /// Skip confirmation prompt (required for non-interactive use).
    #[arg(long, short = 'y')]
    pub yes: bool,
}

const BASE: &str = "/api/sn_sc/servicecatalog";

pub fn list(global: &GlobalFlags, args: CatalogListArgs) -> Result<()> {
    let client = connect(global)?;
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.text {
        query.push(("sysparm_text".into(), v));
    }
    let resp = client.get(&format!("{BASE}/catalogs"), &query)?;
    emit(global, resp)
}

pub fn get(global: &GlobalFlags, args: CatalogGetArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("{BASE}/catalogs/{}", args.sys_id);
    let resp = client.get(&path, &[])?;
    emit(global, resp)
}

pub fn categories(global: &GlobalFlags, args: CatalogCategoriesArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("{BASE}/catalogs/{}/categories", args.catalog_sys_id);
    let mut query: Vec<(String, String)> = Vec::new();
    query.push(("sysparm_limit".into(), args.setlimit.to_string()));
    if let Some(v) = args.offset {
        query.push(("sysparm_offset".into(), v.to_string()));
    }
    if args.top_level_only {
        query.push(("sysparm_top_level_only".into(), "true".into()));
    }
    let resp = client.get(&path, &query)?;
    emit(global, resp)
}

pub fn category(global: &GlobalFlags, args: CatalogCategoryArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("{BASE}/categories/{}", args.sys_id);
    let resp = client.get(&path, &[])?;
    emit(global, resp)
}

pub fn items(global: &GlobalFlags, args: CatalogItemsArgs) -> Result<()> {
    let client = connect(global)?;
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.text {
        query.push(("sysparm_text".into(), v));
    }
    if let Some(v) = args.category {
        query.push(("sysparm_category".into(), v));
    }
    if let Some(v) = args.catalog {
        query.push(("sysparm_catalog".into(), v));
    }
    if let Some(v) = args.item_type {
        query.push(("sysparm_type".into(), v));
    }
    query.push(("sysparm_limit".into(), args.setlimit.to_string()));
    if let Some(v) = args.offset {
        query.push(("sysparm_offset".into(), v.to_string()));
    }
    let resp = client.get(&format!("{BASE}/items"), &query)?;
    emit(global, resp)
}

pub fn item(global: &GlobalFlags, args: CatalogItemArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("{BASE}/items/{}", args.sys_id);
    let resp = client.get(&path, &[])?;
    emit(global, resp)
}

pub fn item_variables(global: &GlobalFlags, args: CatalogItemArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("{BASE}/items/{}/variables", args.sys_id);
    let resp = client.get(&path, &[])?;
    emit(global, resp)
}

pub fn order(global: &GlobalFlags, args: CatalogOrderArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("{BASE}/items/{}/order_now", args.sys_id);
    let body_input = if let Some(d) = args.data {
        BodyInput::Data(d)
    } else if !args.field.is_empty() {
        BodyInput::Fields(args.field)
    } else {
        BodyInput::Data("{}".into())
    };
    let body = build_body(body_input)?;
    let resp = client.post(&path, &[], &body)?;
    emit(global, resp)
}

pub fn add_to_cart(global: &GlobalFlags, args: CatalogOrderArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("{BASE}/items/{}/add_to_cart", args.sys_id);
    let body_input = if let Some(d) = args.data {
        BodyInput::Data(d)
    } else if !args.field.is_empty() {
        BodyInput::Fields(args.field)
    } else {
        BodyInput::Data("{}".into())
    };
    let body = build_body(body_input)?;
    let resp = client.post(&path, &[], &body)?;
    emit(global, resp)
}

pub fn cart(global: &GlobalFlags) -> Result<()> {
    let client = connect(global)?;
    let resp = client.get(&format!("{BASE}/cart"), &[])?;
    emit(global, resp)
}

pub fn cart_update(global: &GlobalFlags, args: CatalogCartUpdateArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("{BASE}/cart/{}", args.cart_item_id);
    let body_input = if let Some(d) = args.data {
        BodyInput::Data(d)
    } else if !args.field.is_empty() {
        BodyInput::Fields(args.field)
    } else {
        BodyInput::None
    };
    let body = build_body(body_input)?;
    let resp = client.put(&path, &[], &body)?;
    emit(global, resp)
}

pub fn cart_remove(global: &GlobalFlags, args: CatalogCartItemArgs) -> Result<()> {
    confirm_destructive(
        args.yes,
        "remove",
        &format!("cart item {}", args.cart_item_id),
    )?;
    let client = connect(global)?;
    let path = format!("{BASE}/cart/{}", args.cart_item_id);
    client.delete(&path, &[])?;
    Ok(())
}

pub fn cart_empty(global: &GlobalFlags, args: CatalogCartEmptyArgs) -> Result<()> {
    // One DELETE discards every line the caller built up, and the cart is not
    // versioned anywhere — there is nothing to restore it from.
    confirm_destructive(args.yes, "empty", &format!("cart {}", args.sys_id))?;
    let client = connect(global)?;
    let path = format!("{BASE}/cart/{}/empty", args.sys_id);
    client.delete(&path, &[])?;
    Ok(())
}

pub fn checkout(global: &GlobalFlags) -> Result<()> {
    let client = connect(global)?;
    let resp = client.post(
        &format!("{BASE}/cart/checkout"),
        &[],
        &serde_json::json!({}),
    )?;
    emit(global, resp)
}

pub fn submit_order(global: &GlobalFlags) -> Result<()> {
    let client = connect(global)?;
    let resp = client.post(
        &format!("{BASE}/cart/submit_order"),
        &[],
        &serde_json::json!({}),
    )?;
    emit(global, resp)
}

pub fn wishlist(global: &GlobalFlags) -> Result<()> {
    let client = connect(global)?;
    let resp = client.get(&format!("{BASE}/wishlist"), &[])?;
    emit(global, resp)
}
