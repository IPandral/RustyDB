-- RustyDB Test Data
-- This file contains sample SQL statements to test the database functionality

-- =====================================================
-- Users Table - Basic user information
-- =====================================================
CREATE TABLE IF NOT EXISTS users (
    id INTEGER PRIMARY KEY,
    username TEXT NOT NULL,
    email TEXT NOT NULL,
    age INTEGER,
    active BOOLEAN
);

INSERT INTO users VALUES (1, 'alice', 'alice@example.com', 28, TRUE);
INSERT INTO users VALUES (2, 'bob', 'bob@example.com', 34, TRUE);
INSERT INTO users VALUES (3, 'charlie', 'charlie@example.com', 22, FALSE);
INSERT INTO users VALUES (4, 'diana', 'diana@example.com', 31, TRUE);
INSERT INTO users VALUES (5, 'eve', 'eve@example.com', 27, TRUE);
INSERT INTO users VALUES (6, 'frank', 'frank@example.com', 45, FALSE);
INSERT INTO users VALUES (7, 'grace', 'grace@example.com', 29, TRUE);
INSERT INTO users VALUES (8, 'henry', 'henry@example.com', 38, TRUE);
INSERT INTO users VALUES (9, 'ivy', 'ivy@example.com', 24, TRUE);
INSERT INTO users VALUES (10, 'jack', 'jack@example.com', 33, FALSE);

-- =====================================================
-- Products Table - E-commerce product catalog
-- =====================================================
CREATE TABLE IF NOT EXISTS products (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    category TEXT,
    price FLOAT,
    in_stock BOOLEAN
);

INSERT INTO products VALUES (1, 'Laptop Pro', 'Electronics', 1299.99, TRUE);
INSERT INTO products VALUES (2, 'Wireless Mouse', 'Electronics', 29.99, TRUE);
INSERT INTO products VALUES (3, 'USB-C Cable', 'Electronics', 12.99, TRUE);
INSERT INTO products VALUES (4, 'Office Chair', 'Furniture', 249.99, TRUE);
INSERT INTO products VALUES (5, 'Standing Desk', 'Furniture', 599.99, FALSE);
INSERT INTO products VALUES (6, 'Monitor 27inch', 'Electronics', 349.99, TRUE);
INSERT INTO products VALUES (7, 'Keyboard Mechanical', 'Electronics', 89.99, TRUE);
INSERT INTO products VALUES (8, 'Desk Lamp', 'Furniture', 45.99, TRUE);
INSERT INTO products VALUES (9, 'Webcam HD', 'Electronics', 79.99, FALSE);
INSERT INTO products VALUES (10, 'Headphones', 'Electronics', 199.99, TRUE);
INSERT INTO products VALUES (11, 'Mouse Pad XL', 'Accessories', 19.99, TRUE);
INSERT INTO products VALUES (12, 'Phone Stand', 'Accessories', 24.99, TRUE);
INSERT INTO products VALUES (13, 'Cable Organizer', 'Accessories', 9.99, TRUE);
INSERT INTO products VALUES (14, 'Notebook', 'Office', 4.99, TRUE);
INSERT INTO products VALUES (15, 'Pen Set', 'Office', 14.99, TRUE);

-- =====================================================
-- Orders Table - Customer orders
-- =====================================================
CREATE TABLE IF NOT EXISTS orders (
    id INTEGER PRIMARY KEY,
    user_id INTEGER,
    product_id INTEGER,
    quantity INTEGER,
    total_price FLOAT
);

INSERT INTO orders VALUES (1, 1, 1, 1, 1299.99);
INSERT INTO orders VALUES (2, 1, 2, 2, 59.98);
INSERT INTO orders VALUES (3, 2, 4, 1, 249.99);
INSERT INTO orders VALUES (4, 3, 7, 1, 89.99);
INSERT INTO orders VALUES (5, 4, 10, 1, 199.99);
INSERT INTO orders VALUES (6, 5, 3, 3, 38.97);
INSERT INTO orders VALUES (7, 6, 6, 2, 699.98);
INSERT INTO orders VALUES (8, 7, 8, 1, 45.99);
INSERT INTO orders VALUES (9, 8, 11, 2, 39.98);
INSERT INTO orders VALUES (10, 9, 14, 10, 49.90);
INSERT INTO orders VALUES (11, 10, 15, 5, 74.95);
INSERT INTO orders VALUES (12, 1, 6, 1, 349.99);
INSERT INTO orders VALUES (13, 2, 2, 1, 29.99);
INSERT INTO orders VALUES (14, 3, 12, 2, 49.98);
INSERT INTO orders VALUES (15, 4, 3, 5, 64.95);

-- =====================================================
-- Employees Table - Company employees
-- =====================================================
CREATE TABLE IF NOT EXISTS employees (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    department TEXT,
    salary FLOAT,
    years_employed INTEGER
);

INSERT INTO employees VALUES (1, 'John Smith', 'Engineering', 85000.00, 5);
INSERT INTO employees VALUES (2, 'Jane Doe', 'Engineering', 92000.00, 7);
INSERT INTO employees VALUES (3, 'Mike Johnson', 'Sales', 65000.00, 3);
INSERT INTO employees VALUES (4, 'Sarah Williams', 'Marketing', 72000.00, 4);
INSERT INTO employees VALUES (5, 'David Brown', 'Engineering', 105000.00, 10);
INSERT INTO employees VALUES (6, 'Emily Davis', 'HR', 58000.00, 2);
INSERT INTO employees VALUES (7, 'Chris Wilson', 'Sales', 78000.00, 6);
INSERT INTO employees VALUES (8, 'Amanda Taylor', 'Marketing', 68000.00, 3);
INSERT INTO employees VALUES (9, 'Robert Martinez', 'Engineering', 95000.00, 8);
INSERT INTO employees VALUES (10, 'Lisa Anderson', 'HR', 62000.00, 4);

-- =====================================================
-- Sample Queries for Testing
-- =====================================================

-- Select all users
-- SELECT * FROM users;

-- Select active users over 25
-- SELECT username, email, age FROM users WHERE age > 25 AND active = TRUE;

-- Select electronics products under $100
-- SELECT name, price FROM products WHERE category = 'Electronics' AND price < 100;

-- Select products sorted by price
-- SELECT * FROM products ORDER BY price DESC LIMIT 5;

-- Select high-value orders
-- SELECT * FROM orders WHERE total_price > 100 ORDER BY total_price DESC;

-- Select engineering employees
-- SELECT name, salary FROM employees WHERE department = 'Engineering' ORDER BY salary DESC;

-- Pattern matching with LIKE
-- SELECT * FROM users WHERE email LIKE '%@example.com';
-- SELECT * FROM products WHERE name LIKE '%Mouse%';
