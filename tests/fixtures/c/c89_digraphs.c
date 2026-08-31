/* Test: C89 digraph support (<: :> <% %> %: %:%:) */
/* Digraphs are alternative tokens for [ ] { } # ## */

/* <: is [ , :> is ] */
int arr<:10:>;

/* <% is { , %> is } */
int foo(void) <%
    int x<:5:> = <% 1, 2, 3, 4, 5 %>;
    return arr<:0:>;
%>

/* %: is # (preprocessor digraph) */
%:define MAGIC 42

/* %:%: is ## (token paste digraph) */
%:define CONCAT(a, b) a %:%: b

int CONCAT(my_, var) = MAGIC;

int main(void) <%
    arr<:0:> = MAGIC;
    arr<:1:> = CONCAT(my_, var);
    return foo() + arr<:0:> + arr<:1:> - 126;
%>
